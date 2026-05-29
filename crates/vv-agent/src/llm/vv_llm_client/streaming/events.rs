use std::collections::BTreeMap;

use serde_json::Value;

use crate::llm::LlmStreamCallback;

use super::tool_calls::StreamingToolCallParts;

pub(super) fn emit_reasoning_delta_event(
    stream_callback: &Option<LlmStreamCallback>,
    reasoning_delta: String,
    reasoning_chars: usize,
) {
    emit_stream_event(
        stream_callback,
        BTreeMap::from([
            (
                "event".to_string(),
                Value::String("reasoning_delta".to_string()),
            ),
            (
                "reasoning_delta".to_string(),
                Value::String(reasoning_delta),
            ),
            (
                "reasoning_chars".to_string(),
                Value::from(reasoning_chars as u64),
            ),
            (
                "estimated_tokens".to_string(),
                Value::from(estimate_stream_tokens(reasoning_chars) as u64),
            ),
        ]),
    );
}

pub(super) fn emit_assistant_delta_event(
    stream_callback: &Option<LlmStreamCallback>,
    content_delta: String,
    content_chars: usize,
) {
    emit_stream_event(
        stream_callback,
        BTreeMap::from([
            (
                "event".to_string(),
                Value::String("assistant_delta".to_string()),
            ),
            ("content_delta".to_string(), Value::String(content_delta)),
            (
                "content_chars".to_string(),
                Value::from(content_chars as u64),
            ),
            (
                "estimated_tokens".to_string(),
                Value::from(estimate_stream_tokens(content_chars) as u64),
            ),
        ]),
    );
}

pub(super) fn emit_tool_stream_event(
    stream_callback: &Option<LlmStreamCallback>,
    event_name: &str,
    default_tool_call_index: usize,
    tool_call: &StreamingToolCallParts,
) {
    let tool_call_index = tool_call.index.unwrap_or(default_tool_call_index);
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
                Value::from(estimate_stream_tokens(tool_call.arguments.chars().count()) as u64),
            ),
        ]),
    );
}

fn emit_stream_event(stream_callback: &Option<LlmStreamCallback>, event: BTreeMap<String, Value>) {
    if let Some(callback) = stream_callback {
        callback(&event);
    }
}

fn estimate_stream_tokens(char_count: usize) -> usize {
    if char_count == 0 {
        0
    } else {
        char_count.div_ceil(4)
    }
}
