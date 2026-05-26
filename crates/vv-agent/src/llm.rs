use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use serde_json::Value;
use thiserror::Error;

use crate::memory::CompactionExhaustedError;
use crate::types::{LLMResponse, Message, MessageRole, TokenUsage, ToolCall};

pub type LlmStreamCallback = Arc<dyn Fn(&BTreeMap<String, Value>) + Send + Sync + 'static>;

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

    fn complete_with_stream(
        &self,
        request: LlmRequest,
        stream_callback: Option<LlmStreamCallback>,
    ) -> Result<LLMResponse, LlmError> {
        let _ = stream_callback;
        self.complete(request)
    }
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

    fn complete_with_stream(
        &self,
        request: LlmRequest,
        stream_callback: Option<LlmStreamCallback>,
    ) -> Result<LLMResponse, LlmError> {
        (**self).complete_with_stream(request, stream_callback)
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
        self.complete_with_stream(request, None)
    }

    fn complete_with_stream(
        &self,
        request: LlmRequest,
        stream_callback: Option<LlmStreamCallback>,
    ) -> Result<LLMResponse, LlmError> {
        let should_stream = stream_callback.is_some()
            || request
                .metadata
                .get("stream")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        let mut chat_request = vv_llm::ChatRequest {
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
        if should_stream {
            chat_request.options.stream = Some(true);
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|error| LlmError::Request(error.to_string()))?;
        if should_stream {
            return runtime.block_on(collect_vv_llm_stream(
                Arc::clone(&self.chat_client),
                chat_request,
                stream_callback,
            ));
        }

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

#[derive(Debug, Default)]
struct StreamingToolCallParts {
    id: String,
    name: String,
    arguments: String,
}

async fn collect_vv_llm_stream(
    chat_client: Arc<dyn vv_llm::ChatClient>,
    request: vv_llm::ChatRequest,
    stream_callback: Option<LlmStreamCallback>,
) -> Result<LLMResponse, LlmError> {
    let model = request.model.clone();
    let mut stream = chat_client
        .create_stream(request)
        .await
        .map_err(|error| LlmError::Request(error.to_string()))?;
    let mut content = String::new();
    let mut reasoning_content = String::new();
    let mut raw_content = Vec::new();
    let mut usage = None;
    let mut tool_order = Vec::<String>::new();
    let mut tool_calls = BTreeMap::<String, StreamingToolCallParts>::new();
    let mut active_tool_call_key = None::<String>;

    while let Some(delta) = stream.next().await {
        let delta = delta.map_err(|error| LlmError::Request(error.to_string()))?;
        if let Some(delta_usage) = delta.usage {
            usage = Some(delta_usage);
        }
        if let Some(raw) = delta.raw_content {
            raw_content.push(raw);
        }
        if !delta.reasoning_content.is_empty() {
            reasoning_content.push_str(&delta.reasoning_content);
            emit_stream_event(
                &stream_callback,
                BTreeMap::from([
                    (
                        "event".to_string(),
                        Value::String("reasoning_delta".to_string()),
                    ),
                    (
                        "reasoning_delta".to_string(),
                        Value::String(delta.reasoning_content),
                    ),
                    (
                        "reasoning_chars".to_string(),
                        Value::from(reasoning_content.chars().count() as u64),
                    ),
                    (
                        "estimated_tokens".to_string(),
                        Value::from(estimate_stream_tokens(reasoning_content.len()) as u64),
                    ),
                ]),
            );
        }
        if !delta.content.is_empty() {
            content.push_str(&delta.content);
            emit_stream_event(
                &stream_callback,
                BTreeMap::from([
                    (
                        "event".to_string(),
                        Value::String("assistant_delta".to_string()),
                    ),
                    ("content_delta".to_string(), Value::String(delta.content)),
                    (
                        "content_chars".to_string(),
                        Value::from(content.chars().count() as u64),
                    ),
                    (
                        "estimated_tokens".to_string(),
                        Value::from(estimate_stream_tokens(content.len()) as u64),
                    ),
                ]),
            );
        }
        for (tool_call_index, tool_call_delta) in delta.tool_calls.into_iter().enumerate() {
            let key = resolve_stream_tool_call_key(
                &tool_call_delta,
                tool_call_index,
                active_tool_call_key.as_deref(),
            );
            active_tool_call_key = Some(key.clone());
            if !tool_calls.contains_key(&key) {
                tool_order.push(key.clone());
                tool_calls.insert(
                    key.clone(),
                    StreamingToolCallParts {
                        id: if tool_call_delta.id.is_empty() {
                            key.clone()
                        } else {
                            tool_call_delta.id.clone()
                        },
                        ..StreamingToolCallParts::default()
                    },
                );
            }
            let Some(slot) = tool_calls.get_mut(&key) else {
                continue;
            };
            if !tool_call_delta.id.is_empty() {
                slot.id = tool_call_delta.id.clone();
            }
            let had_name = !slot.name.is_empty();
            if !tool_call_delta.name.is_empty() {
                slot.name = tool_call_delta.name.clone();
            }
            if !tool_call_delta.arguments.is_empty() {
                slot.arguments.push_str(&tool_call_delta.arguments);
            }
            if !had_name && !slot.name.is_empty() {
                emit_tool_stream_event(
                    &stream_callback,
                    "tool_call_started",
                    tool_call_index,
                    slot,
                );
            }
            if !tool_call_delta.arguments.is_empty() {
                emit_tool_stream_event(
                    &stream_callback,
                    "tool_call_progress",
                    tool_call_index,
                    slot,
                );
            }
        }
        if delta.done {
            break;
        }
    }

    let mut raw = crate::types::Metadata::new();
    raw.insert("model".to_string(), Value::String(model));
    raw.insert("stream_collected".to_string(), Value::Bool(true));
    if !reasoning_content.is_empty() {
        raw.insert(
            "reasoning_content".to_string(),
            Value::String(reasoning_content),
        );
    }
    if !raw_content.is_empty() {
        raw.insert("raw_content".to_string(), Value::Array(raw_content));
    }
    let token_usage = usage.map(from_vv_llm_usage).unwrap_or_default();
    Ok(LLMResponse {
        content,
        tool_calls: tool_order
            .into_iter()
            .filter_map(|key| tool_calls.remove(&key))
            .filter(|parts| !parts.name.is_empty())
            .map(|parts| {
                from_vv_llm_tool_call(vv_llm::ToolCall::function(
                    parts.id,
                    parts.name,
                    parts.arguments,
                ))
            })
            .collect(),
        raw,
        token_usage,
    })
}

fn resolve_stream_tool_call_key(
    tool_call: &vv_llm::ToolCall,
    index: usize,
    active_tool_call_key: Option<&str>,
) -> String {
    if !tool_call.id.is_empty() {
        return tool_call.id.clone();
    }
    if let Some(active) = active_tool_call_key {
        return active.to_string();
    }
    format!("call_stream_{index}")
}

fn emit_stream_event(stream_callback: &Option<LlmStreamCallback>, event: BTreeMap<String, Value>) {
    if let Some(callback) = stream_callback {
        callback(&event);
    }
}

fn emit_tool_stream_event(
    stream_callback: &Option<LlmStreamCallback>,
    event_name: &str,
    tool_call_index: usize,
    tool_call: &StreamingToolCallParts,
) {
    emit_stream_event(
        stream_callback,
        BTreeMap::from([
            ("event".to_string(), Value::String(event_name.to_string())),
            (
                "tool_call_id".to_string(),
                Value::String(tool_call.id.clone()),
            ),
            (
                "tool_call_index".to_string(),
                Value::from(tool_call_index as u64),
            ),
            (
                "function_name".to_string(),
                Value::String(tool_call.name.clone()),
            ),
            (
                "arguments_chars".to_string(),
                Value::from(tool_call.arguments.chars().count() as u64),
            ),
            (
                "estimated_tokens".to_string(),
                Value::from(estimate_stream_tokens(tool_call.arguments.len()) as u64),
            ),
        ]),
    );
}

fn estimate_stream_tokens(text_length: usize) -> usize {
    if text_length == 0 {
        0
    } else {
        text_length.div_ceil(4)
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
