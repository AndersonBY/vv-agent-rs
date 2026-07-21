use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::events::{MemoryCompactMode, MemoryCompactTrigger, ReservedOutputSource, RunEvent};

use super::{with_selected_payload_metadata, RuntimeEventContext};

const PROVIDER_METADATA_FIELDS: &[&str] = &["memory_provider_results", "memory_provider_errors"];

pub(super) fn map_memory_compact_started(
    payload: &BTreeMap<String, Value>,
    context: &RuntimeEventContext,
) -> Option<RunEvent> {
    let event = RunEvent::memory_compact_started_observed(
        &context.run_id,
        &context.trace_id,
        &context.agent_name,
        payload_u32(payload, "cycle")?,
        payload_usize(payload, "message_count")?,
        payload_optional_u64(payload, "estimated_tokens")?,
        payload_enum::<MemoryCompactTrigger>(payload, "trigger")?,
        payload_u64(payload, "configured_threshold")?,
        payload_u64(payload, "effective_threshold")?,
        payload_u64(payload, "microcompact_threshold")?,
        payload_u64(payload, "model_context_window")?,
        payload_nullable_u64(payload, "model_max_output_tokens")?,
        payload_u64(payload, "reserved_output_tokens")?,
        payload_enum::<ReservedOutputSource>(payload, "reserved_output_source")?,
        payload_u64(payload, "autocompact_buffer_tokens")?,
    );
    with_observed_identity(
        with_selected_payload_metadata(event, payload, PROVIDER_METADATA_FIELDS),
        payload,
    )
}

pub(super) fn map_memory_compact_completed(
    payload: &BTreeMap<String, Value>,
    context: &RuntimeEventContext,
) -> Option<RunEvent> {
    let event = RunEvent::memory_compact_completed_observed(
        &context.run_id,
        &context.trace_id,
        &context.agent_name,
        payload_u32(payload, "cycle")?,
        payload_usize(payload, "before_count")?,
        payload_usize(payload, "after_count")?,
        payload_optional_u64(payload, "summary_tokens")?,
        payload_enum::<MemoryCompactMode>(payload, "mode")?,
        payload.get("changed").and_then(Value::as_bool)?,
    );
    with_observed_identity(
        with_selected_payload_metadata(event, payload, PROVIDER_METADATA_FIELDS),
        payload,
    )
}

fn with_observed_identity(event: RunEvent, payload: &BTreeMap<String, Value>) -> Option<RunEvent> {
    event
        .with_observed_identity(
            payload.get("event_id")?.as_str()?,
            payload.get("created_at")?.as_f64()?,
        )
        .ok()
}

fn payload_u64(payload: &BTreeMap<String, Value>, field: &str) -> Option<u64> {
    payload.get(field).and_then(Value::as_u64)
}

fn payload_u32(payload: &BTreeMap<String, Value>, field: &str) -> Option<u32> {
    u32::try_from(payload_u64(payload, field)?).ok()
}

fn payload_usize(payload: &BTreeMap<String, Value>, field: &str) -> Option<usize> {
    usize::try_from(payload_u64(payload, field)?).ok()
}

fn payload_optional_u64(payload: &BTreeMap<String, Value>, field: &str) -> Option<Option<u64>> {
    match payload.get(field) {
        None | Some(Value::Null) => Some(None),
        Some(value) => value.as_u64().map(Some),
    }
}

fn payload_nullable_u64(payload: &BTreeMap<String, Value>, field: &str) -> Option<Option<u64>> {
    match payload.get(field)? {
        Value::Null => Some(None),
        value => value.as_u64().map(Some),
    }
}

fn payload_enum<T: DeserializeOwned>(payload: &BTreeMap<String, Value>, field: &str) -> Option<T> {
    serde_json::from_value(payload.get(field)?.clone()).ok()
}
