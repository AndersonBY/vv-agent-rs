use serde_json::Value;

use crate::types::{LLMResponse, Message, ToolExecutionResult};

pub(super) fn assistant_message_from_response(response: &LLMResponse) -> Message {
    let mut message = Message::assistant(response.content.clone());
    message.tool_calls = response.tool_calls.clone();
    message
}

pub(super) fn extract_final_message(result: &ToolExecutionResult) -> String {
    result
        .metadata
        .get("final_message")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            serde_json::from_str::<Value>(&result.content)
                .ok()
                .and_then(|value| {
                    value
                        .get("message")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
        })
        .unwrap_or_else(|| result.content.clone())
}

pub(super) fn extract_wait_reason(result: &ToolExecutionResult) -> String {
    result
        .metadata
        .get("question")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| result.content.clone())
}
