mod endpoints;
mod model_rules;
mod prompt_cache;
mod request;
mod response;
mod streaming;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::memory::token_utils::count_messages_tokens;
use crate::types::LLMResponse;

use super::{LlmClient, LlmError, LlmRequest, LlmStreamCallback};
use endpoints::{annotate_endpoint_response, EndpointChatClient};
use model_rules::{resolve_request_options, should_preserve_reasoning_chain, should_use_stream};
use prompt_cache::{
    apply_prompt_cache_to_chat_request, endpoint_type_for_prompt_cache,
    request_metadata_for_prompt_cache,
};
use request::{prepare_messages_for_model, prepare_reasoning_chain_messages, to_vv_llm_message};
use response::{from_vv_llm_response, UsageEstimateContext};
use streaming::collect_vv_llm_stream;

pub type EndpointClientSpec = (String, Box<dyn vv_llm::ChatClient>);
pub type NamedEndpointClientSpec = (String, String, Box<dyn vv_llm::ChatClient>);

#[derive(Clone)]
pub struct VvLlmClient {
    pub backend: String,
    pub selected_model: String,
    pub model_id: String,
    pub timeout_seconds: f64,
    pub debug_dump_dir: Option<PathBuf>,
    pub max_retries_per_endpoint: usize,
    pub backoff_seconds: f64,
    pub randomize_endpoints: bool,
    request_counter: Arc<Mutex<u64>>,
    endpoint_order_counter: Arc<Mutex<u64>>,
    preferred_endpoint_id: Arc<Mutex<Option<String>>>,
    endpoint_clients: Vec<EndpointChatClient>,
}

impl VvLlmClient {
    pub fn new(
        backend: impl Into<String>,
        selected_model: impl Into<String>,
        model_id: impl Into<String>,
        chat_client: Box<dyn vv_llm::ChatClient>,
        timeout_seconds: f64,
    ) -> Self {
        let model_id = model_id.into();
        Self::new_with_named_endpoint_clients(
            backend,
            selected_model,
            model_id.clone(),
            vec![(model_id.clone(), model_id, chat_client)],
            timeout_seconds,
        )
    }

    pub fn new_with_endpoint_clients(
        backend: impl Into<String>,
        selected_model: impl Into<String>,
        model_id: impl Into<String>,
        endpoint_clients: Vec<EndpointClientSpec>,
        timeout_seconds: f64,
    ) -> Self {
        Self::new_with_named_endpoint_clients(
            backend,
            selected_model,
            model_id,
            endpoint_clients
                .into_iter()
                .map(|(model_id, chat_client)| (model_id.clone(), model_id, chat_client))
                .collect(),
            timeout_seconds,
        )
    }

    pub fn new_with_named_endpoint_clients(
        backend: impl Into<String>,
        selected_model: impl Into<String>,
        model_id: impl Into<String>,
        endpoint_clients: Vec<NamedEndpointClientSpec>,
        timeout_seconds: f64,
    ) -> Self {
        Self {
            backend: backend.into(),
            selected_model: selected_model.into(),
            model_id: model_id.into(),
            timeout_seconds,
            debug_dump_dir: None,
            max_retries_per_endpoint: 3,
            backoff_seconds: 2.0,
            randomize_endpoints: true,
            request_counter: Arc::new(Mutex::new(0)),
            endpoint_order_counter: Arc::new(Mutex::new(0)),
            preferred_endpoint_id: Arc::new(Mutex::new(None)),
            endpoint_clients: endpoint_clients
                .into_iter()
                .map(|(endpoint_id, model_id, chat_client)| {
                    EndpointChatClient::new(endpoint_id, model_id, chat_client)
                })
                .collect(),
        }
    }

    pub fn provider_name(&self) -> &'static str {
        self.endpoint_clients
            .first()
            .map(|endpoint| endpoint.chat_client.provider_name())
            .unwrap_or("unknown")
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    pub fn endpoint_count(&self) -> usize {
        self.endpoint_clients.len()
    }

    pub fn randomize_endpoints(&self) -> bool {
        self.randomize_endpoints
    }

    pub fn with_randomize_endpoints(mut self, randomize_endpoints: bool) -> Self {
        self.randomize_endpoints = randomize_endpoints;
        self
    }

    pub fn with_debug_dump_dir(mut self, debug_dump_dir: impl AsRef<Path>) -> Self {
        self.debug_dump_dir = Some(debug_dump_dir.as_ref().to_path_buf());
        self
    }

    pub fn with_retry_policy(
        mut self,
        max_retries_per_endpoint: usize,
        backoff_seconds: f64,
    ) -> Self {
        self.max_retries_per_endpoint = max_retries_per_endpoint.max(1);
        self.backoff_seconds = backoff_seconds.max(0.0);
        self
    }
}

impl LlmClient for VvLlmClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        self.complete_with_stream(request, None)
    }

    fn clone_with_debug_dump_dir(&self, debug_dump_dir: &Path) -> Option<Arc<dyn LlmClient>> {
        Some(Arc::new(self.clone().with_debug_dump_dir(debug_dump_dir)))
    }

    fn complete_with_stream(
        &self,
        request: LlmRequest,
        stream_callback: Option<LlmStreamCallback>,
    ) -> Result<LLMResponse, LlmError> {
        if self.endpoint_clients.is_empty() {
            return Err(LlmError::Request(
                "No endpoint targets configured".to_string(),
            ));
        }

        let mut errors = Vec::new();
        for endpoint in self.ordered_endpoint_clients() {
            for attempt in 1..=self.max_retries_per_endpoint.max(1) {
                match self.complete_with_endpoint(
                    &endpoint,
                    request.clone(),
                    stream_callback.clone(),
                ) {
                    Ok(response) => {
                        self.remember_preferred_endpoint(&endpoint.endpoint_id);
                        return Ok(response);
                    }
                    Err(error) => {
                        errors.push(format!(
                            "{}: {error} (attempt {attempt})",
                            endpoint.endpoint_id
                        ));
                        if attempt < self.max_retries_per_endpoint {
                            self.sleep_backoff(attempt);
                        }
                    }
                }
            }
        }
        Err(LlmError::Request(format!(
            "all endpoint targets failed: {}",
            errors.join("; ")
        )))
    }
}

impl VvLlmClient {
    fn complete_with_endpoint(
        &self,
        endpoint: &EndpointChatClient,
        request: LlmRequest,
        stream_callback: Option<LlmStreamCallback>,
    ) -> Result<LLMResponse, LlmError> {
        let effective_model = self.effective_model_for_endpoint(&request.model, endpoint);
        let request_options = resolve_request_options(&effective_model);
        let request_model = request_options.model.clone();
        let preserve_reasoning_chain = should_preserve_reasoning_chain(&[
            &request.model,
            &self.selected_model,
            &endpoint.model_id,
            &request_model,
        ]);
        let should_stream = stream_callback.is_some()
            || request
                .metadata
                .get("stream")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            || should_use_stream(&effective_model);
        let request_metadata = request_metadata_for_prompt_cache(&request);
        let estimated_prompt_tokens = count_messages_tokens(&request.messages, &request_model);
        let mut chat_request = vv_llm::ChatRequest {
            model: request_model.clone(),
            messages: prepare_reasoning_chain_messages(
                request
                    .messages
                    .into_iter()
                    .map(to_vv_llm_message)
                    .collect(),
                preserve_reasoning_chain,
            ),
            options: vv_llm::ChatRequestOptions {
                temperature: request_options.temperature,
                max_tokens: request_options.max_tokens,
                stream: None,
            },
            tools: request
                .tools
                .into_iter()
                .map(request::to_vv_llm_tool)
                .collect(),
            tool_choice: request
                .metadata
                .get("tool_choice")
                .and_then(Value::as_str)
                .map(str::to_string),
            extra_body: request_options.extra_body,
        };
        apply_prompt_cache_to_chat_request(
            &endpoint_type_for_prompt_cache(&self.backend, endpoint.chat_client.provider_name()),
            &request_model,
            &request_metadata,
            &mut chat_request,
        );
        chat_request.messages = prepare_messages_for_model(chat_request.messages, &request_model);
        if should_stream {
            chat_request.options.stream = Some(true);
        }
        self.dump_request_messages(&chat_request.messages, &request_model);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|error| LlmError::Request(error.to_string()))?;
        if should_stream {
            let mut response = runtime.block_on(collect_vv_llm_stream(
                Arc::clone(&endpoint.chat_client),
                chat_request,
                stream_callback,
                Some(UsageEstimateContext {
                    model: request_model.clone(),
                    prompt_tokens: estimated_prompt_tokens,
                }),
            ))?;
            annotate_endpoint_response(&mut response, endpoint, &request_model, should_stream);
            return Ok(response);
        }

        let response = runtime
            .block_on(endpoint.chat_client.create_completion(chat_request))
            .map_err(|error| LlmError::Request(error.to_string()))?;

        let mut response = from_vv_llm_response(
            response,
            Some(UsageEstimateContext {
                model: request_model.clone(),
                prompt_tokens: estimated_prompt_tokens,
            }),
        );
        annotate_endpoint_response(&mut response, endpoint, &request_model, should_stream);
        Ok(response)
    }
}

impl std::fmt::Debug for VvLlmClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VvLlmClient")
            .field("backend", &self.backend)
            .field("selected_model", &self.selected_model)
            .field("model_id", &self.model_id)
            .field("provider_name", &self.provider_name())
            .field("timeout_seconds", &self.timeout_seconds)
            .field("debug_dump_dir", &self.debug_dump_dir)
            .field("max_retries_per_endpoint", &self.max_retries_per_endpoint)
            .field("backoff_seconds", &self.backoff_seconds)
            .field("randomize_endpoints", &self.randomize_endpoints)
            .finish()
    }
}
