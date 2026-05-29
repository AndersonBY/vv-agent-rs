use serde_json::Value;

use crate::llm::anthropic_prompt_cache::apply_claude_prompt_cache;
use crate::llm::LlmRequest;
use crate::types::MessageRole;

pub(super) fn request_metadata_for_prompt_cache(request: &LlmRequest) -> Value {
    let mut metadata = request
        .metadata
        .as_object()
        .cloned()
        .unwrap_or_else(serde_json::Map::new);
    if let Some(system_metadata) = request
        .messages
        .iter()
        .find(|message| message.role == MessageRole::System)
        .map(|message| &message.metadata)
        .filter(|metadata| !metadata.is_empty())
    {
        metadata.extend(system_metadata.clone());
    }
    Value::Object(metadata)
}

pub(super) fn apply_prompt_cache_to_chat_request(
    endpoint_type: &str,
    model: &str,
    metadata: &Value,
    chat_request: &mut vv_llm::ChatRequest,
) {
    let messages = chat_request
        .messages
        .iter()
        .map(vv_llm_message_to_cache_json)
        .collect::<Vec<_>>();
    let tools = chat_request
        .tools
        .iter()
        .map(vv_llm_tool_to_cache_json)
        .collect::<Vec<_>>();
    let (planned_messages, planned_tools, planned_extra_body) = apply_claude_prompt_cache(
        endpoint_type,
        model,
        &messages,
        &tools,
        Some(&chat_request.extra_body),
        Some(metadata),
    );
    apply_planned_message_content(&mut chat_request.messages, planned_messages);
    apply_planned_tool_cache_control(&mut chat_request.tools, planned_tools);
    if let Some(extra_body) = planned_extra_body {
        chat_request.extra_body = extra_body;
    }
}

pub(super) fn endpoint_type_for_prompt_cache(backend: &str, provider_name: &str) -> String {
    let normalized_provider = provider_name.trim().to_ascii_lowercase();
    if matches!(
        normalized_provider.as_str(),
        "anthropic" | "anthropic_vertex"
    ) {
        return normalized_provider;
    }
    backend.trim().to_ascii_lowercase()
}

fn vv_llm_message_to_cache_json(message: &vv_llm::Message) -> Value {
    let mut object = serde_json::Map::new();
    object.insert(
        "role".to_string(),
        Value::String(message.role.as_str().to_string()),
    );
    object.insert(
        "content".to_string(),
        Value::Array(
            message
                .content
                .iter()
                .map(vv_llm_content_to_cache_json)
                .collect(),
        ),
    );
    if let Some(name) = message.name.as_ref().filter(|name| !name.is_empty()) {
        object.insert("name".to_string(), Value::String(name.clone()));
    }
    if let Some(tool_call_id) = message
        .tool_call_id
        .as_ref()
        .filter(|tool_call_id| !tool_call_id.is_empty())
    {
        object.insert(
            "tool_call_id".to_string(),
            Value::String(tool_call_id.clone()),
        );
    }
    if let Some(reasoning_content) = message
        .reasoning_content
        .as_ref()
        .filter(|reasoning_content| !reasoning_content.is_empty())
    {
        object.insert(
            "reasoning_content".to_string(),
            Value::String(reasoning_content.clone()),
        );
    }
    if !message.tool_calls.is_empty() {
        let tool_calls = message
            .tool_calls
            .iter()
            .map(vv_llm_tool_call_to_openai_json)
            .collect::<Vec<_>>();
        object.insert("tool_calls".to_string(), Value::Array(tool_calls));
        let content = object
            .entry("content".to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Some(blocks) = content.as_array_mut() {
            blocks.extend(
                message
                    .tool_calls
                    .iter()
                    .map(vv_llm_tool_call_to_anthropic_cache_json),
            );
        }
    }
    Value::Object(object)
}

fn vv_llm_content_to_cache_json(content: &vv_llm::MessageContent) -> Value {
    match content {
        vv_llm::MessageContent::Text {
            text,
            cache_control,
        } => {
            let mut object = serde_json::Map::new();
            object.insert("type".to_string(), Value::String("text".to_string()));
            object.insert("text".to_string(), Value::String(text.clone()));
            if let Some(cache_control) = cache_control {
                object.insert("cache_control".to_string(), cache_control.clone());
            }
            Value::Object(object)
        }
        vv_llm::MessageContent::ImageUrl { url } => serde_json::json!({
            "type": "image_url",
            "image_url": {"url": url},
        }),
    }
}

fn vv_llm_tool_call_to_openai_json(tool_call: &vv_llm::ToolCall) -> Value {
    serde_json::json!({
        "id": tool_call.id,
        "type": "function",
        "function": {
            "name": tool_call.name,
            "arguments": tool_call.arguments,
        },
    })
}

fn vv_llm_tool_call_to_anthropic_cache_json(tool_call: &vv_llm::ToolCall) -> Value {
    let input = serde_json::from_str::<Value>(&tool_call.arguments)
        .unwrap_or_else(|_| Value::String(tool_call.arguments.clone()));
    serde_json::json!({
        "type": "tool_use",
        "id": tool_call.id,
        "name": tool_call.name,
        "input": input,
    })
}

fn vv_llm_tool_to_cache_json(tool: &vv_llm::ChatTool) -> Value {
    let mut object = serde_json::Map::new();
    object.insert("type".to_string(), Value::String("function".to_string()));
    object.insert(
        "function".to_string(),
        serde_json::json!({
            "name": tool.name,
            "description": tool.description.clone().unwrap_or_default(),
            "parameters": tool.parameters,
        }),
    );
    if let Some(cache_control) = &tool.cache_control {
        object.insert("cache_control".to_string(), cache_control.clone());
    }
    Value::Object(object)
}

fn apply_planned_message_content(messages: &mut [vv_llm::Message], planned_messages: Vec<Value>) {
    for (message, planned_message) in messages.iter_mut().zip(planned_messages) {
        let Some(content) = planned_message.get("content") else {
            continue;
        };
        message.content = cache_content_to_vv_llm_content(content);
    }
}

fn cache_content_to_vv_llm_content(content: &Value) -> Vec<vv_llm::MessageContent> {
    match content {
        Value::Array(items) => items
            .iter()
            .filter_map(cache_block_to_vv_llm_content)
            .collect(),
        Value::String(text) => vec![vv_llm::MessageContent::text(text.clone())],
        _ => Vec::new(),
    }
}

fn cache_block_to_vv_llm_content(block: &Value) -> Option<vv_llm::MessageContent> {
    let object = block.as_object()?;
    let block_type = object
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("text")
        .to_ascii_lowercase();
    match block_type.as_str() {
        "text" => Some(vv_llm_text_content_from_cache_block(object)),
        "image_url" => object
            .get("image_url")
            .and_then(|image_url| image_url.get("url"))
            .and_then(Value::as_str)
            .map(|url| vv_llm::MessageContent::ImageUrl {
                url: url.to_string(),
            }),
        "tool_result" => {
            let text = cache_block_content_text(object.get("content"));
            Some(vv_llm_text_content_with_optional_cache_control(
                text,
                object.get("cache_control").cloned(),
            ))
        }
        _ => None,
    }
}

fn vv_llm_text_content_from_cache_block(
    object: &serde_json::Map<String, Value>,
) -> vv_llm::MessageContent {
    let text = object
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    vv_llm_text_content_with_optional_cache_control(text, object.get("cache_control").cloned())
}

fn vv_llm_text_content_with_optional_cache_control(
    text: String,
    cache_control: Option<Value>,
) -> vv_llm::MessageContent {
    match cache_control {
        Some(cache_control) => vv_llm::MessageContent::text_with_cache_control(text, cache_control),
        None => vv_llm::MessageContent::text(text),
    }
}

fn cache_block_content_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(_)) | Some(Value::Object(_)) => {
            serde_json::to_string(content.unwrap()).unwrap_or_default()
        }
        Some(Value::Bool(value)) => value.to_string(),
        Some(Value::Number(value)) => value.to_string(),
        Some(Value::Null) | None => String::new(),
    }
}

fn apply_planned_tool_cache_control(tools: &mut [vv_llm::ChatTool], planned_tools: Vec<Value>) {
    for (tool, planned_tool) in tools.iter_mut().zip(planned_tools) {
        if let Some(cache_control) = planned_tool.get("cache_control") {
            tool.cache_control = Some(cache_control.clone());
        }
    }
}
