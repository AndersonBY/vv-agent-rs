mod construction;
mod endpoints;
mod execution;
mod model_rules;
mod prompt_cache;
mod request;
mod response;
mod streaming;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::types::LLMResponse;

use super::{LlmClient, LlmError, LlmRequest, LlmStreamCallback};
use endpoints::EndpointChatClient;

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
