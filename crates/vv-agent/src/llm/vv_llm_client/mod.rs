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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EndpointAttemptAction {
    RetrySameEndpoint,
    Failover,
    Abort,
}

#[derive(Debug)]
struct EndpointAttemptError {
    error: LlmError,
    action: EndpointAttemptAction,
}

impl EndpointAttemptError {
    fn abort(error: LlmError) -> Self {
        Self {
            error,
            action: EndpointAttemptAction::Abort,
        }
    }

    fn retry(error: LlmError) -> Self {
        Self {
            error,
            action: EndpointAttemptAction::RetrySameEndpoint,
        }
    }

    fn from_provider(error: vv_llm::VvLlmError) -> Self {
        let action = match &error {
            vv_llm::VvLlmError::Configuration(_) | vv_llm::VvLlmError::Serialization(_) => {
                EndpointAttemptAction::Abort
            }
            vv_llm::VvLlmError::ModelNotFound { .. } | vv_llm::VvLlmError::EndpointNotFound(_) => {
                EndpointAttemptAction::Failover
            }
            vv_llm::VvLlmError::Http(_) => EndpointAttemptAction::RetrySameEndpoint,
            vv_llm::VvLlmError::Provider(message) => provider_error_action(message),
        };
        Self {
            error: LlmError::Request(error.to_string()),
            action,
        }
    }

    fn into_llm_error(self) -> LlmError {
        self.error
    }
}

impl std::fmt::Display for EndpointAttemptError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(formatter)
    }
}

fn provider_error_action(message: &str) -> EndpointAttemptAction {
    if let Some(status) = leading_http_status(message) {
        return match status {
            408 | 429 | 500 | 502 | 503 | 504 => EndpointAttemptAction::RetrySameEndpoint,
            400 | 413 | 422 => EndpointAttemptAction::Abort,
            401 | 403 | 404 => EndpointAttemptAction::Failover,
            400..=499 => EndpointAttemptAction::Failover,
            500..=599 => EndpointAttemptAction::RetrySameEndpoint,
            _ => EndpointAttemptAction::Failover,
        };
    }

    let message = message.to_ascii_lowercase();
    if [
        "http error",
        "connection",
        "timed out",
        "timeout",
        "rate limit",
        "overloaded",
        "temporarily unavailable",
    ]
    .iter()
    .any(|candidate| message.contains(candidate))
    {
        return EndpointAttemptAction::RetrySameEndpoint;
    }
    if [
        "prompt is too long",
        "context_length_exceeded",
        "maximum context length",
        "request too large",
        "too many tokens",
        "invalid request",
    ]
    .iter()
    .any(|candidate| message.contains(candidate))
    {
        return EndpointAttemptAction::Abort;
    }
    EndpointAttemptAction::Failover
}

fn leading_http_status(message: &str) -> Option<u16> {
    let status = message.split_whitespace().next()?.parse::<u16>().ok()?;
    (100..=599).contains(&status).then_some(status)
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
        if let Some(settings) = request.model_settings.as_ref() {
            settings.validate().map_err(LlmError::Request)?;
        }
        if !self.timeout_seconds.is_finite() || self.timeout_seconds <= 0.0 {
            return Err(LlmError::Request(
                "provider timeout_seconds must be a finite positive number".to_string(),
            ));
        }
        if request
            .model_settings
            .as_ref()
            .is_some_and(|settings| !settings.extra_headers.is_empty())
        {
            return Err(LlmError::Request(
                "ModelSettings.extra_headers is not supported by vv-llm 0.2.3; configure headers on the provider endpoint instead"
                    .to_string(),
            ));
        }
        if request
            .model_settings
            .as_ref()
            .is_some_and(|settings| !settings.extra_args.is_empty())
        {
            return Err(LlmError::Request(
                "ModelSettings.extra_args is not supported by vv-llm 0.2.3; use extra_body or a custom model client instead"
                    .to_string(),
            ));
        }

        let (max_attempts, backoff_seconds) = request
            .model_settings
            .as_ref()
            .and_then(|settings| settings.retry.as_ref())
            .map(|retry| {
                (
                    retry.max_attempts.max(1) as usize,
                    retry.backoff_seconds.max(0.0),
                )
            })
            .unwrap_or((self.max_retries_per_endpoint.max(1), self.backoff_seconds));

        let mut errors = Vec::new();
        for endpoint in self.ordered_endpoint_clients() {
            for attempt in 1..=max_attempts {
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
                        match error.action {
                            EndpointAttemptAction::RetrySameEndpoint if attempt < max_attempts => {
                                self.sleep_backoff(backoff_seconds, attempt);
                                continue;
                            }
                            EndpointAttemptAction::RetrySameEndpoint
                            | EndpointAttemptAction::Failover => break,
                            EndpointAttemptAction::Abort => return Err(error.into_llm_error()),
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

#[cfg(test)]
mod retry_classification_tests {
    use super::*;

    #[test]
    fn provider_statuses_have_explicit_retry_dispositions() {
        assert_eq!(
            provider_error_action("429 Too Many Requests"),
            EndpointAttemptAction::RetrySameEndpoint
        );
        assert_eq!(
            provider_error_action("400 Bad Request"),
            EndpointAttemptAction::Abort
        );
        assert_eq!(
            provider_error_action("401 Unauthorized"),
            EndpointAttemptAction::Failover
        );
        assert_eq!(
            provider_error_action("opaque provider failure"),
            EndpointAttemptAction::Failover
        );
    }
}
