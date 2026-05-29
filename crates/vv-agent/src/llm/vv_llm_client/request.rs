use serde_json::Value;

use crate::types::{Message, MessageRole, ToolCall};

pub(super) fn to_vv_llm_message(message: Message) -> vv_llm::Message {
    let role = match message.role {
        MessageRole::System => vv_llm::MessageRole::System,
        MessageRole::User => vv_llm::MessageRole::User,
        MessageRole::Assistant => vv_llm::MessageRole::Assistant,
        MessageRole::Tool => vv_llm::MessageRole::Tool,
    };
    let mut content = Vec::new();
    if !message.content.is_empty() {
        content.push(vv_llm::MessageContent::text(message.content));
    }
    if let Some(image_url) = message.image_url.filter(|image_url| !image_url.is_empty()) {
        content.push(vv_llm::MessageContent::ImageUrl { url: image_url });
    }
    vv_llm::Message {
        role,
        content,
        name: message.name.filter(|name| !name.is_empty()),
        tool_call_id: message
            .tool_call_id
            .filter(|tool_call_id| !tool_call_id.is_empty()),
        tool_calls: message
            .tool_calls
            .into_iter()
            .map(to_vv_llm_tool_call)
            .collect(),
        reasoning_content: message.reasoning_content.filter(|value| !value.is_empty()),
    }
}

fn to_vv_llm_tool_call(tool_call: ToolCall) -> vv_llm::ToolCall {
    let mut vv_tool_call = vv_llm::ToolCall::function(
        tool_call.id,
        tool_call.name,
        Value::Object(tool_call.arguments.into_iter().collect()).to_string(),
    );
    vv_tool_call.extra_content = tool_call.extra_content;
    vv_tool_call
}

pub(super) fn prepare_messages_for_model(
    messages: Vec<vv_llm::Message>,
    model: &str,
) -> Vec<vv_llm::Message> {
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
                vec![vv_llm::MessageContent::text(content)]
            };
            message.name = None;
            message.tool_call_id = None;
            message.tool_calls.clear();
            message
        })
        .collect()
}

pub(super) fn prepare_reasoning_chain_messages(
    mut messages: Vec<vv_llm::Message>,
    preserve_reasoning_chain: bool,
) -> Vec<vv_llm::Message> {
    if !preserve_reasoning_chain {
        return messages;
    }
    for message in &mut messages {
        if message.role == vv_llm::MessageRole::Assistant && message.reasoning_content.is_none() {
            message.reasoning_content = Some(String::new());
        }
    }
    messages
}

pub(super) fn to_vv_llm_tool(tool: Value) -> vv_llm::ChatTool {
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
