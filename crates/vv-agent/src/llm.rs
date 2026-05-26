use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use thiserror::Error;

use crate::memory::CompactionExhaustedError;
use crate::types::{LLMResponse, Message, MessageRole, TokenUsage, ToolCall};

#[derive(Debug, Clone, PartialEq)]
pub struct LlmRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Vec<Value>,
    pub metadata: Value,
}

impl LlmRequest {
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> Self {
        Self {
            model: model.into(),
            messages,
            tools: Vec::new(),
            metadata: Value::Null,
        }
    }
}

#[derive(Debug, Error)]
pub enum LlmError {
    #[error("scripted response queue is empty")]
    ScriptExhausted,
    #[error("{0}")]
    CompactionExhausted(CompactionExhaustedError),
    #[error("llm request failed: {0}")]
    Request(String),
}

pub trait LlmClient: Send + Sync {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointTarget {
    pub endpoint_id: String,
    pub api_key: String,
    pub api_base: String,
    pub endpoint_type: String,
    pub model_id: String,
}

impl EndpointTarget {
    pub fn new(
        endpoint_id: impl Into<String>,
        api_key: impl Into<String>,
        api_base: impl Into<String>,
        endpoint_type: impl Into<String>,
        model_id: impl Into<String>,
    ) -> Self {
        Self {
            endpoint_id: endpoint_id.into(),
            api_key: api_key.into(),
            api_base: api_base.into(),
            endpoint_type: endpoint_type.into(),
            model_id: model_id.into(),
        }
    }
}

#[derive(Clone)]
pub struct ScriptedLlmClient {
    responses: Arc<Mutex<VecDeque<LLMResponse>>>,
}

impl ScriptedLlmClient {
    pub fn new(responses: Vec<LLMResponse>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(VecDeque::from(responses))),
        }
    }

    pub fn push_response(&self, response: LLMResponse) {
        if let Ok(mut queue) = self.responses.lock() {
            queue.push_back(response);
        }
    }
}

impl std::fmt::Debug for ScriptedLlmClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptedLlmClient").finish_non_exhaustive()
    }
}

impl LlmClient for ScriptedLlmClient {
    fn complete(&self, _request: LlmRequest) -> Result<LLMResponse, LlmError> {
        let mut queue = self
            .responses
            .lock()
            .map_err(|_| LlmError::Request("scripted response queue poisoned".to_string()))?;
        Ok(queue.pop_front().unwrap_or_else(|| LLMResponse::new("")))
    }
}

impl<T> LlmClient for Arc<T>
where
    T: LlmClient + ?Sized,
{
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        (**self).complete(request)
    }
}

#[derive(Clone)]
pub struct VvLlmClient {
    pub backend: String,
    pub selected_model: String,
    pub model_id: String,
    pub timeout_seconds: f64,
    chat_client: Arc<dyn vv_llm::ChatClient>,
}

impl VvLlmClient {
    pub fn new(
        backend: impl Into<String>,
        selected_model: impl Into<String>,
        model_id: impl Into<String>,
        chat_client: Box<dyn vv_llm::ChatClient>,
        timeout_seconds: f64,
    ) -> Self {
        Self {
            backend: backend.into(),
            selected_model: selected_model.into(),
            model_id: model_id.into(),
            timeout_seconds,
            chat_client: Arc::from(chat_client),
        }
    }

    pub fn provider_name(&self) -> &'static str {
        self.chat_client.provider_name()
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }
}

impl LlmClient for VvLlmClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        let chat_request = vv_llm::ChatRequest {
            model: if request.model.is_empty() {
                self.model_id.clone()
            } else {
                request.model
            },
            messages: request
                .messages
                .into_iter()
                .map(to_vv_llm_message)
                .collect(),
            options: vv_llm::ChatRequestOptions::default(),
            tools: request.tools.into_iter().map(to_vv_llm_tool).collect(),
            tool_choice: request
                .metadata
                .get("tool_choice")
                .and_then(Value::as_str)
                .map(str::to_string),
        };
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|error| LlmError::Request(error.to_string()))?;
        let response = runtime
            .block_on(self.chat_client.create_completion(chat_request))
            .map_err(|error| LlmError::Request(error.to_string()))?;

        Ok(from_vv_llm_response(response))
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
            .finish()
    }
}

fn to_vv_llm_message(message: Message) -> vv_llm::Message {
    let role = match message.role {
        MessageRole::System => vv_llm::MessageRole::System,
        MessageRole::User => vv_llm::MessageRole::User,
        MessageRole::Assistant => vv_llm::MessageRole::Assistant,
        MessageRole::Tool => vv_llm::MessageRole::Tool,
    };
    let mut content = Vec::new();
    if !message.content.is_empty() {
        content.push(vv_llm::MessageContent::Text {
            text: message.content,
        });
    }
    if let Some(image_url) = message.image_url {
        content.push(vv_llm::MessageContent::ImageUrl { url: image_url });
    }
    vv_llm::Message {
        role,
        content,
        name: message.name,
        tool_call_id: message.tool_call_id,
        tool_calls: message
            .tool_calls
            .into_iter()
            .map(to_vv_llm_tool_call)
            .collect(),
    }
}

fn to_vv_llm_tool_call(tool_call: ToolCall) -> vv_llm::ToolCall {
    vv_llm::ToolCall::function(
        tool_call.id,
        tool_call.name,
        Value::Object(tool_call.arguments.into_iter().collect()).to_string(),
    )
}

fn to_vv_llm_tool(tool: Value) -> vv_llm::ChatTool {
    let function = tool.get("function").unwrap_or(&tool);
    let name = function
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let description = function
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let parameters = function
        .get("parameters")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({"type": "object"}));
    vv_llm::ChatTool::function(name, description, parameters)
}

fn from_vv_llm_response(response: vv_llm::ChatResponse) -> LLMResponse {
    let mut raw = crate::types::Metadata::new();
    raw.insert("id".to_string(), Value::String(response.id));
    raw.insert("model".to_string(), Value::String(response.model));
    LLMResponse {
        content: response.content,
        tool_calls: response
            .tool_calls
            .into_iter()
            .map(from_vv_llm_tool_call)
            .collect(),
        raw,
        token_usage: response.usage.map(from_vv_llm_usage).unwrap_or_default(),
    }
}

fn from_vv_llm_tool_call(tool_call: vv_llm::ToolCall) -> ToolCall {
    ToolCall::from_raw_arguments(
        tool_call.id,
        tool_call.name,
        Value::String(tool_call.arguments),
    )
}

fn from_vv_llm_usage(usage: vv_llm::ChatUsage) -> TokenUsage {
    let raw = serde_json::to_value(&usage).unwrap_or(Value::Null);
    TokenUsage {
        prompt_tokens: usage.prompt_tokens.unwrap_or_default() as u64,
        completion_tokens: usage.completion_tokens.unwrap_or_default() as u64,
        total_tokens: usage.total_tokens.unwrap_or_default() as u64,
        input_tokens: usage.prompt_tokens.unwrap_or_default() as u64,
        output_tokens: usage.completion_tokens.unwrap_or_default() as u64,
        raw,
        ..TokenUsage::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_openai_function_schema_to_vv_llm_tool() {
        let tool = to_vv_llm_tool(serde_json::json!({
            "type": "function",
            "function": {
                "name": "task_finish",
                "description": "Finish task",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "message": {"type": "string"}
                    },
                    "required": ["message"]
                }
            }
        }));

        assert_eq!(tool.name, "task_finish");
        assert_eq!(tool.description.as_deref(), Some("Finish task"));
        assert_eq!(tool.parameters["properties"]["message"]["type"], "string");
    }
}
