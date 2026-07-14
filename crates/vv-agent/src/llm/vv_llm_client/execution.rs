use std::sync::Arc;

use serde_json::Value;

use crate::memory::token_utils::count_messages_tokens;
use crate::types::LLMResponse;

use super::endpoints::{annotate_endpoint_response, EndpointChatClient};
use super::model_rules::{
    resolve_request_options, should_preserve_reasoning_chain, should_use_stream,
};
use super::prompt_cache::{
    apply_prompt_cache_to_chat_request, endpoint_type_for_prompt_cache,
    request_metadata_for_prompt_cache,
};
use super::request::{
    prepare_messages_for_model, prepare_reasoning_chain_messages, to_vv_llm_message,
};
use super::response::{from_vv_llm_response, UsageEstimateContext};
use super::streaming::collect_vv_llm_stream;
use super::{EndpointAttemptError, VvLlmClient};
use crate::llm::{LlmError, LlmRequest, LlmStreamCallback};
use crate::model_settings::ToolChoice;

impl VvLlmClient {
    pub(super) fn complete_with_endpoint(
        &self,
        endpoint: &EndpointChatClient,
        request: LlmRequest,
        stream_callback: Option<LlmStreamCallback>,
    ) -> Result<LLMResponse, EndpointAttemptError> {
        let effective_model = self.effective_model_for_endpoint(&request.model, endpoint);
        let endpoint_provider = endpoint.chat_client.provider_name();
        let request_options = resolve_request_options(
            &self.backend,
            endpoint_provider,
            &effective_model,
            request.model_settings.as_ref(),
        );
        let request_timeout = request_options
            .timeout
            .unwrap_or_else(|| std::time::Duration::from_secs_f64(self.timeout_seconds));
        let request_model = request_options.model.clone();
        let preserve_reasoning_chain = should_preserve_reasoning_chain(
            &self.backend,
            &[
                &request.model,
                &self.selected_model,
                &endpoint.model_id,
                &request_model,
            ],
        );
        let should_stream = stream_callback.is_some()
            || request
                .metadata
                .get("stream")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            || should_use_stream(&effective_model);
        let request_metadata = request_metadata_for_prompt_cache(&request);
        let estimated_prompt_tokens = count_messages_tokens(&request.messages, &request_model);
        let (request_tools, tool_choice) =
            apply_tool_choice(request.tools, request_options.tool_choice.as_ref())
                .map_err(EndpointAttemptError::abort)?;
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
                ..vv_llm::ChatRequestOptions::default()
            },
            tools: request_tools
                .into_iter()
                .map(super::request::to_vv_llm_tool)
                .collect(),
            tool_choice,
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
            .map_err(|error| EndpointAttemptError::abort(LlmError::Request(error.to_string())))?;
        if should_stream {
            let completion = collect_vv_llm_stream(
                Arc::clone(&endpoint.chat_client),
                chat_request,
                stream_callback,
                Some(UsageEstimateContext {
                    model: request_model.clone(),
                    prompt_tokens: estimated_prompt_tokens,
                }),
            );
            let mut response = runtime
                .block_on(async { tokio::time::timeout(request_timeout, completion).await })
                .map_err(|_| request_timeout_error(request_timeout))??;
            annotate_endpoint_response(&mut response, endpoint, &request_model, should_stream);
            return Ok(response);
        }

        let completion = endpoint.chat_client.create_completion(chat_request);
        let response = runtime
            .block_on(async { tokio::time::timeout(request_timeout, completion).await })
            .map_err(|_| request_timeout_error(request_timeout))?
            .map_err(EndpointAttemptError::from_provider)?;

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

fn request_timeout_error(timeout: std::time::Duration) -> EndpointAttemptError {
    EndpointAttemptError::retry(LlmError::Request(format!(
        "request timed out after {:.3} seconds",
        timeout.as_secs_f64()
    )))
}

fn apply_tool_choice(
    tools: Vec<Value>,
    tool_choice: Option<&ToolChoice>,
) -> Result<(Vec<Value>, Option<String>), LlmError> {
    match tool_choice {
        None => Ok((tools, None)),
        Some(ToolChoice::Auto) => Ok((tools, Some("auto".to_string()))),
        Some(ToolChoice::Required) => Ok((tools, Some("required".to_string()))),
        Some(ToolChoice::None) => Ok((Vec::new(), None)),
        Some(ToolChoice::Tool(name)) => {
            let selected = tools
                .into_iter()
                .filter(|tool| tool_payload_name(tool) == Some(name.as_str()))
                .collect::<Vec<_>>();
            if selected.is_empty() {
                return Err(LlmError::Request(format!(
                    "tool_choice refers to unknown tool: {name}"
                )));
            }
            Ok((selected, Some("required".to_string())))
        }
    }
}

fn tool_payload_name(tool: &Value) -> Option<&str> {
    tool.get("function")
        .and_then(|function| function.get("name"))
        .and_then(Value::as_str)
}
