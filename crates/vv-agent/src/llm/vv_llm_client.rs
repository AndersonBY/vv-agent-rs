use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use serde_json::Value;

use crate::memory::token_utils::{count_messages_tokens, count_tokens};
use crate::types::{LLMResponse, Message, MessageRole, TokenUsage, ToolCall};

use super::{LlmClient, LlmError, LlmRequest, LlmStreamCallback};

pub type EndpointClientSpec = (String, Box<dyn vv_llm::ChatClient>);
pub type NamedEndpointClientSpec = (String, String, Box<dyn vv_llm::ChatClient>);

const STREAM_MODEL_PREFIXES: &[&str] = &[
    "qwen3", "claude", "gemini", "kimi", "glm-4.", "glm-5", "gpt-5", "minimax",
];
const STREAM_MODEL_EXACT: &[&str] = &[
    "deepseek-reasoner",
    "deepseek-r1-tools",
    "deepseek-v4-flash",
    "deepseek-v4-pro",
];
const CLAUDE_THINKING_MODELS: &[&str] = &[
    "claude-3-7-sonnet-thinking",
    "claude-opus-4-20250514-thinking",
    "claude-opus-4-1-20250805-thinking",
    "claude-sonnet-4-20250514-thinking",
    "claude-sonnet-4-5-20250929-thinking",
    "claude-opus-4-5-20251101-thinking",
    "claude-opus-4-6-thinking",
    "claude-sonnet-4-6-thinking",
];

#[derive(Clone)]
pub struct VvLlmClient {
    pub backend: String,
    pub selected_model: String,
    pub model_id: String,
    pub timeout_seconds: f64,
    pub debug_dump_dir: Option<PathBuf>,
    request_counter: Arc<Mutex<u64>>,
    endpoint_clients: Vec<EndpointChatClient>,
}

#[derive(Clone)]
struct EndpointChatClient {
    endpoint_id: String,
    model_id: String,
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
            request_counter: Arc::new(Mutex::new(0)),
            endpoint_clients: endpoint_clients
                .into_iter()
                .map(|(endpoint_id, model_id, chat_client)| EndpointChatClient {
                    endpoint_id,
                    model_id,
                    chat_client: Arc::from(chat_client),
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

    pub fn with_debug_dump_dir(mut self, debug_dump_dir: impl AsRef<Path>) -> Self {
        self.debug_dump_dir = Some(debug_dump_dir.as_ref().to_path_buf());
        self
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
        if self.endpoint_clients.is_empty() {
            return Err(LlmError::Request(
                "No endpoint targets configured".to_string(),
            ));
        }

        let mut errors = Vec::new();
        for endpoint in &self.endpoint_clients {
            match self.complete_with_endpoint(endpoint, request.clone(), stream_callback.clone()) {
                Ok(response) => return Ok(response),
                Err(error) => errors.push(format!("{}: {error}", endpoint.endpoint_id)),
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
        let should_stream = stream_callback.is_some()
            || request
                .metadata
                .get("stream")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            || should_use_stream(&effective_model);
        let estimated_prompt_tokens = count_messages_tokens(&request.messages, &request_model);
        let mut chat_request = vv_llm::ChatRequest {
            model: request_model.clone(),
            messages: prepare_messages_for_model(
                request
                    .messages
                    .into_iter()
                    .map(to_vv_llm_message)
                    .collect(),
                &request_model,
            ),
            options: vv_llm::ChatRequestOptions {
                temperature: request_options.temperature,
                max_tokens: request_options.max_tokens,
                stream: None,
            },
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

    fn effective_model_for_endpoint(
        &self,
        requested_model: &str,
        endpoint: &EndpointChatClient,
    ) -> String {
        let requested_model = requested_model.trim();
        if requested_model.is_empty()
            || requested_model == self.model_id
            || requested_model == self.selected_model
        {
            endpoint.model_id.clone()
        } else {
            requested_model.to_string()
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct ResolvedRequestOptions {
    model: String,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
}

fn resolve_request_options(model: &str) -> ResolvedRequestOptions {
    let mut resolved_model = model.to_string();
    let mut normalized_model = resolved_model.to_ascii_lowercase();
    let mut temperature = None;
    let mut max_tokens = None;

    if STREAM_MODEL_EXACT
        .iter()
        .any(|candidate| normalized_model == *candidate)
    {
        temperature = Some(0.6);
    } else if CLAUDE_THINKING_MODELS
        .iter()
        .any(|candidate| normalized_model == *candidate)
    {
        resolved_model = remove_suffix_case_insensitive(&resolved_model, "-thinking");
        normalized_model = resolved_model.to_ascii_lowercase();
        temperature = Some(1.0);
        max_tokens = Some(20_000);
    }

    if normalized_model.starts_with("gemini-3") {
        temperature.get_or_insert(1.0);
        if normalized_model == "gemini-3-pro" || normalized_model == "gemini-3-flash" {
            resolved_model = format!("{resolved_model}-preview");
        }
    }

    ResolvedRequestOptions {
        model: resolved_model,
        temperature,
        max_tokens,
    }
}

fn remove_suffix_case_insensitive(value: &str, suffix: &str) -> String {
    if value.to_ascii_lowercase().ends_with(suffix) {
        value[..value.len().saturating_sub(suffix.len())].to_string()
    } else {
        value.to_string()
    }
}

fn annotate_endpoint_response(
    response: &mut LLMResponse,
    endpoint: &EndpointChatClient,
    model_id: &str,
    stream_mode: bool,
) {
    response.raw.insert(
        "used_endpoint_id".to_string(),
        Value::String(endpoint.endpoint_id.clone()),
    );
    response.raw.insert(
        "used_model_id".to_string(),
        Value::String(model_id.to_string()),
    );
    response
        .raw
        .insert("stream_mode".to_string(), Value::Bool(stream_mode));
}

fn should_use_stream(model: &str) -> bool {
    let normalized = model.trim().to_ascii_lowercase();
    STREAM_MODEL_EXACT
        .iter()
        .any(|candidate| normalized == *candidate)
        || STREAM_MODEL_PREFIXES
            .iter()
            .any(|prefix| normalized.starts_with(prefix))
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
            .finish()
    }
}

impl VvLlmClient {
    fn dump_request_messages(&self, messages: &[vv_llm::Message], model_name: &str) {
        let Some(dump_dir) = &self.debug_dump_dir else {
            return;
        };
        let Ok(mut request_counter) = self.request_counter.lock() else {
            return;
        };
        *request_counter += 1;
        let request_index = *request_counter;

        let _ = std::fs::create_dir_all(dump_dir);
        let filename = format!(
            "request_{request_index:03}_{}.json",
            safe_model_filename(model_name)
        );
        let payload = serde_json::json!({
            "request_index": request_index,
            "model": model_name,
            "message_count": messages.len(),
            "messages": messages,
        });
        if let Ok(content) = serde_json::to_string_pretty(&payload) {
            let _ = std::fs::write(dump_dir.join(filename), content);
        }
    }
}

fn safe_model_filename(model_name: &str) -> String {
    let safe = model_name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    let safe = safe.trim_matches('_');
    if safe.is_empty() {
        "model".to_string()
    } else {
        safe.to_string()
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

fn prepare_messages_for_model(messages: Vec<vv_llm::Message>, model: &str) -> Vec<vv_llm::Message> {
    if !model.to_ascii_lowercase().starts_with("minimax") {
        return messages;
    }

    let mut seen_system = false;
    messages
        .into_iter()
        .map(|mut message| {
            if message.role != vv_llm::MessageRole::System {
                return message;
            }
            if !seen_system {
                seen_system = true;
                return message;
            }

            let prefix = if message.name.as_deref() == Some("memory_summary") {
                "[memory_summary]\n"
            } else {
                ""
            };
            let content = format!("{prefix}{}", message.text_content().unwrap_or_default())
                .trim()
                .to_string();
            message.role = vv_llm::MessageRole::User;
            message.content = if content.is_empty() {
                Vec::new()
            } else {
                vec![vv_llm::MessageContent::Text { text: content }]
            };
            message.name = None;
            message.tool_call_id = None;
            message.tool_calls.clear();
            message
        })
        .collect()
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

#[derive(Debug, Clone)]
struct UsageEstimateContext {
    model: String,
    prompt_tokens: u64,
}

fn from_vv_llm_response(
    response: vv_llm::ChatResponse,
    estimate: Option<UsageEstimateContext>,
) -> LLMResponse {
    let mut raw = crate::types::Metadata::new();
    raw.insert("id".to_string(), Value::String(response.id));
    raw.insert("model".to_string(), Value::String(response.model));
    let token_usage = response.usage.map(from_vv_llm_usage).unwrap_or_else(|| {
        estimate_missing_usage(&response.content, &response.tool_calls, estimate)
    });
    raw.insert("usage".to_string(), token_usage.raw.clone());
    LLMResponse {
        content: response.content,
        tool_calls: response
            .tool_calls
            .into_iter()
            .map(from_vv_llm_tool_call)
            .collect(),
        raw,
        token_usage,
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

fn estimate_missing_usage(
    content: &str,
    tool_calls: &[vv_llm::ToolCall],
    estimate: Option<UsageEstimateContext>,
) -> TokenUsage {
    let Some(estimate) = estimate else {
        return TokenUsage::default();
    };
    let completion_payload = completion_payload_for_usage(content, tool_calls);
    let completion_tokens = count_tokens(&completion_payload, &estimate.model);
    let total_tokens = estimate.prompt_tokens + completion_tokens;
    let raw = serde_json::json!({
        "prompt_tokens": estimate.prompt_tokens,
        "completion_tokens": completion_tokens,
        "total_tokens": total_tokens,
    });
    TokenUsage {
        prompt_tokens: estimate.prompt_tokens,
        completion_tokens,
        total_tokens,
        input_tokens: estimate.prompt_tokens,
        output_tokens: completion_tokens,
        raw,
        ..TokenUsage::default()
    }
}

fn completion_payload_for_usage(content: &str, tool_calls: &[vv_llm::ToolCall]) -> String {
    if tool_calls.is_empty() {
        return content.to_string();
    }
    let mut payload = content.to_string();
    if let Ok(tool_payload) = serde_json::to_string(tool_calls) {
        if !payload.is_empty() {
            payload.push('\n');
        }
        payload.push_str(&tool_payload);
    }
    payload
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
    estimate: Option<UsageEstimateContext>,
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
            collect_raw_content(&mut raw_content, raw);
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
    let completion_payload = completion_payload_for_usage(
        &content,
        &tool_order
            .iter()
            .filter_map(|key| {
                tool_calls.get(key).map(|parts| {
                    vv_llm::ToolCall::function(&parts.id, &parts.name, &parts.arguments)
                })
            })
            .collect::<Vec<_>>(),
    );
    let token_usage = usage
        .map(from_vv_llm_usage)
        .unwrap_or_else(|| estimate_missing_usage(&completion_payload, &[], estimate));
    raw.insert("usage".to_string(), token_usage.raw.clone());
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

fn collect_raw_content(blocks: &mut Vec<Value>, chunk: Value) {
    match chunk {
        Value::Array(items) => {
            for item in items {
                collect_raw_content(blocks, item);
            }
        }
        Value::Object(object) => collect_raw_content_object(blocks, object),
        _ => {}
    }
}

fn collect_raw_content_object(blocks: &mut Vec<Value>, object: serde_json::Map<String, Value>) {
    let chunk_type = object
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match chunk_type {
        "thinking_delta" => {
            let index = find_or_create_raw_block(
                blocks,
                "thinking",
                &[("thinking", ""), ("signature", "")],
            );
            append_raw_block_string(blocks, index, "thinking", object.get("thinking"));
        }
        "signature_delta" => {
            let index = find_or_create_raw_block(
                blocks,
                "thinking",
                &[("thinking", ""), ("signature", "")],
            );
            append_raw_block_string(blocks, index, "signature", object.get("signature"));
        }
        "text_delta" => {
            let index = find_or_create_raw_block(blocks, "text", &[("text", "")]);
            append_raw_block_string(blocks, index, "text", object.get("text"));
        }
        "input_json_delta" => {}
        "thinking" | "text" | "tool_use" => {
            if !raw_block_exists(blocks, &object) {
                blocks.push(Value::Object(object));
            }
        }
        _ => blocks.push(Value::Object(object)),
    }
}

fn find_or_create_raw_block(
    blocks: &mut Vec<Value>,
    block_type: &str,
    defaults: &[(&str, &str)],
) -> usize {
    if let Some(index) = blocks.iter().position(|block| {
        block
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|candidate| candidate == block_type)
    }) {
        return index;
    }

    let mut block = serde_json::Map::new();
    block.insert("type".to_string(), Value::String(block_type.to_string()));
    for (key, value) in defaults {
        block.insert((*key).to_string(), Value::String((*value).to_string()));
    }
    blocks.push(Value::Object(block));
    blocks.len() - 1
}

fn append_raw_block_string(
    blocks: &mut [Value],
    index: usize,
    key: &str,
    addition: Option<&Value>,
) {
    let addition = addition.and_then(Value::as_str).unwrap_or_default();
    let Some(block) = blocks.get_mut(index).and_then(Value::as_object_mut) else {
        return;
    };
    let mut value = block
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    value.push_str(addition);
    block.insert(key.to_string(), Value::String(value));
}

fn raw_block_exists(blocks: &[Value], candidate: &serde_json::Map<String, Value>) -> bool {
    let candidate_type = candidate.get("type");
    let candidate_id = candidate.get("id");
    blocks.iter().any(|block| {
        let Some(block) = block.as_object() else {
            return false;
        };
        if block.get("type") != candidate_type {
            return false;
        }
        if let Some(candidate_id) = candidate_id {
            return block.get("id") == Some(candidate_id);
        }
        block == candidate
    })
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

    #[test]
    fn should_use_stream_matches_python_model_rules() {
        assert!(should_use_stream("deepseek-v4-pro"));
        assert!(should_use_stream("MiniMax-M2.1"));
        assert!(should_use_stream("claude-sonnet-4-6-thinking"));
        assert!(!should_use_stream("gpt-4o-mini"));
    }
}
