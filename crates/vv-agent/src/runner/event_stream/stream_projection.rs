use std::collections::BTreeMap;

use serde_json::Value;

use crate::events::{RunEvent, RunEventPayload};

use super::RuntimeEventContext;

const JSON_SAFE_INTEGER_MAX: u64 = (1_u64 << 53) - 1;
const CANONICAL_CYCLE_FIELD: &str = "cycle_index";
const CANONICAL_STREAM_IDENTITY_FIELDS: &[&str] = &[
    "agent_name",
    "child_run_id",
    "child_session_id",
    "parent_run_id",
    "parent_tool_call_id",
    "run_id",
    "session_id",
    "sub_agent_name",
    "task_id",
    "trace_id",
];
const ASSISTANT_DELTA_FIELDS: &[&str] = &[
    "content_chars",
    "content_delta",
    "delta",
    "estimated_tokens",
    "event",
];
const REASONING_DELTA_FIELDS: &[&str] = &[
    "estimated_tokens",
    "event",
    "reasoning_chars",
    "reasoning_delta",
];
const TOOL_STREAM_FIELDS: &[&str] = &[
    "arguments_chars",
    "estimated_tokens",
    "event",
    "function_name",
    "tool_call_id",
    "tool_call_index",
];

pub(in crate::runner) fn map_stream_event(
    payload: &BTreeMap<String, Value>,
    context: &RuntimeEventContext,
) -> Option<RunEvent> {
    let event = payload
        .get("event")
        .or_else(|| payload.get("type"))
        .and_then(Value::as_str)?;
    if let Some(canonical) = canonical_sub_agent_stream_payload(event, payload) {
        let _ = context.consume_trusted_stream_receipt(&canonical);
        return None;
    }
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

pub(super) fn map_canonical_sub_agent_stream_event(
    stream_event: &str,
    canonical: &BTreeMap<String, Value>,
) -> Option<RunEvent> {
    let cycle_index = positive_cycle(canonical, CANONICAL_CYCLE_FIELD)?;
    let payload = project_stream_payload(stream_event, canonical)?;
    let mut event = RunEvent::new(
        required_text(canonical, "run_id")?,
        required_text(canonical, "trace_id")?,
        required_text(canonical, "agent_name")?,
        Some(cycle_index),
        payload,
    )
    .with_session_id(required_text(canonical, "session_id")?)
    .with_parent_run_id(required_text(canonical, "parent_run_id")?);
    for (key, value) in canonical {
        event = event.with_metadata(key, value.clone());
    }
    Some(event)
}

pub(super) fn canonical_sub_agent_stream_payload(
    event: &str,
    payload: &BTreeMap<String, Value>,
) -> Option<BTreeMap<String, Value>> {
    let producer_fields = match event {
        "assistant_delta" => ASSISTANT_DELTA_FIELDS,
        "reasoning_delta" => REASONING_DELTA_FIELDS,
        "tool_call_started" | "tool_call_progress" => TOOL_STREAM_FIELDS,
        _ => return None,
    };
    let mut canonical = payload
        .iter()
        .filter(|(key, _)| {
            producer_fields.contains(&key.as_str())
                || CANONICAL_STREAM_IDENTITY_FIELDS.contains(&key.as_str())
                || key.as_str() == CANONICAL_CYCLE_FIELD
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<BTreeMap<_, _>>();
    canonical.insert("event".to_string(), Value::String(event.to_string()));
    if !CANONICAL_STREAM_IDENTITY_FIELDS
        .iter()
        .all(|key| required_text(&canonical, key).is_some())
        || positive_cycle(&canonical, CANONICAL_CYCLE_FIELD).is_none()
    {
        return None;
    }
    for (left, right) in [
        ("run_id", "child_run_id"),
        ("session_id", "child_session_id"),
        ("agent_name", "sub_agent_name"),
    ] {
        if canonical.get(left) != canonical.get(right) {
            return None;
        }
    }
    Some(canonical)
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
