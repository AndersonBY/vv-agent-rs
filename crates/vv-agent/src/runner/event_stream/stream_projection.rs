use std::collections::BTreeMap;

use serde_json::Value;

use crate::events::{RunEvent, RunEventPayload};

use super::RuntimeEventContext;

const JSON_SAFE_INTEGER_MAX: u64 = (1_u64 << 53) - 1;

pub(crate) fn map_stream_event(
    payload: &BTreeMap<String, Value>,
    context: &RuntimeEventContext,
) -> Option<RunEvent> {
    let event = payload.get("event").and_then(Value::as_str)?;
    let cycle_index = positive_cycle(payload, "cycle")?;
    let projected = project_stream_payload(event, payload)?;
    Some(context.attach(RunEvent::new(
        &context.run_id,
        &context.trace_id,
        &context.agent_name,
        Some(cycle_index),
        projected,
    )))
}

fn project_stream_payload(
    event: &str,
    payload: &BTreeMap<String, Value>,
) -> Option<RunEventPayload> {
    match event {
        "assistant_delta" => Some(RunEventPayload::AssistantDelta {
            delta: payload
                .get("content_delta")
                .and_then(Value::as_str)
                .or_else(|| payload.get("delta").and_then(Value::as_str))?
                .to_string(),
            content_chars: optional_counter(payload, "content_chars")?,
            estimated_tokens: optional_counter(payload, "estimated_tokens")?,
        }),
        "reasoning_delta" => Some(RunEventPayload::ReasoningDelta {
            delta: payload.get("reasoning_delta")?.as_str()?.to_string(),
            reasoning_chars: optional_counter(payload, "reasoning_chars")?,
            estimated_tokens: optional_counter(payload, "estimated_tokens")?,
        }),
        "tool_call_started" => Some(RunEventPayload::ModelToolCallStarted {
            tool_call_id: required_text(payload, "tool_call_id")?.to_string(),
            tool_call_index: optional_counter(payload, "tool_call_index")?,
            tool_name: required_text(payload, "function_name")?.to_string(),
            arguments_chars: optional_counter(payload, "arguments_chars")?,
            estimated_tokens: optional_counter(payload, "estimated_tokens")?,
        }),
        "tool_call_progress" => Some(RunEventPayload::ModelToolCallProgress {
            tool_call_id: required_text(payload, "tool_call_id")?.to_string(),
            tool_call_index: optional_counter(payload, "tool_call_index")?,
            tool_name: required_text(payload, "function_name")?.to_string(),
            arguments_chars: optional_counter(payload, "arguments_chars")?,
            estimated_tokens: optional_counter(payload, "estimated_tokens")?,
        }),
        _ => None,
    }
}

fn required_text<'a>(payload: &'a BTreeMap<String, Value>, key: &str) -> Option<&'a str> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
}

fn positive_cycle(payload: &BTreeMap<String, Value>, key: &str) -> Option<u32> {
    let value = payload.get(key)?.as_u64()?;
    u32::try_from(value).ok().filter(|value| *value > 0)
}

fn optional_counter(payload: &BTreeMap<String, Value>, key: &str) -> Option<Option<u64>> {
    match payload.get(key) {
        None | Some(Value::Null) => Some(None),
        Some(value) => value
            .as_u64()
            .filter(|value| *value <= JSON_SAFE_INTEGER_MAX)
            .map(Some),
    }
}
