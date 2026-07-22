use serde_json::{json, Value};
use vv_agent::RunEvent;

const FIXTURE: &str = include_str!("fixtures/parity/run_events_invalid.json");

fn contract() -> Value {
    serde_json::from_str(FIXTURE).expect("run event invalid fixture")
}

#[test]
fn invalid_run_event_inputs_are_rejected() {
    let contract = contract();
    let cases = contract["reject"].as_array().expect("reject cases");
    let ids = cases
        .iter()
        .filter_map(|case| case["id"].as_str())
        .collect::<std::collections::BTreeSet<_>>();
    for id in [
        "memory_compact_trigger_is_unknown",
        "memory_compact_capacity_is_negative",
        "memory_compact_reserved_output_source_is_unknown",
        "memory_compact_mode_is_unknown",
        "memory_compact_changed_is_not_boolean",
    ] {
        assert!(ids.contains(id), "missing invalid memory case {id}");
    }
    for case in cases {
        let result = serde_json::from_value::<RunEvent>(case["input"].clone());
        assert!(result.is_err(), "{}", case["id"]);
    }
}

#[test]
fn memory_compaction_known_non_nullable_fields_reject_explicit_null() {
    for field in [
        "trigger",
        "configured_threshold",
        "effective_threshold",
        "microcompact_threshold",
        "model_context_window",
        "reserved_output_tokens",
        "reserved_output_source",
        "autocompact_buffer_tokens",
    ] {
        let mut value = current_memory_compact_started();
        value[field] = Value::Null;
        assert!(
            serde_json::from_value::<RunEvent>(value).is_err(),
            "{field} accepted explicit null"
        );
    }

    for field in ["mode", "changed"] {
        let mut value = current_memory_compact_completed();
        value[field] = Value::Null;
        assert!(
            serde_json::from_value::<RunEvent>(value).is_err(),
            "{field} accepted explicit null"
        );
    }
}

#[test]
fn memory_compaction_model_output_capability_accepts_explicit_null() {
    let value = current_memory_compact_started();

    let event: RunEvent = serde_json::from_value(value.clone()).expect("nullable capability");
    assert_eq!(serde_json::to_value(event).expect("serialize event"), value);
}

fn current_memory_compact_started() -> Value {
    json!({
        "version": "v1",
        "type": "memory_compact_started",
        "event_id": "evt_nullable_model_capability",
        "run_id": "run_nullable_model_capability",
        "trace_id": "trace_nullable_model_capability",
        "created_at": 1,
        "message_count": 3,
        "trigger": "full_threshold",
        "configured_threshold": 250000,
        "effective_threshold": 250000,
        "microcompact_threshold": 187500,
        "model_context_window": 1000000,
        "model_max_output_tokens": null,
        "reserved_output_tokens": 16000,
        "reserved_output_source": "framework_fallback",
        "autocompact_buffer_tokens": 13000
    })
}

fn current_memory_compact_completed() -> Value {
    json!({
        "version": "v1",
        "type": "memory_compact_completed",
        "event_id": "evt_invalid_memory_completed",
        "run_id": "run_invalid_memory",
        "trace_id": "trace_invalid_memory",
        "created_at": 1,
        "before_count": 3,
        "after_count": 2,
        "mode": "summary",
        "changed": true
    })
}
