use serde_json::Value;

pub(super) fn vv_llm_message_to_cache_json(message: &vv_llm::Message) -> Value {
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

pub(super) fn vv_llm_tool_to_cache_json(tool: &vv_llm::ChatTool) -> Value {
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
