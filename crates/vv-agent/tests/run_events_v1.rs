use serde_json::json;
use vv_agent::{AgentStatus, RunEvent, RunEventPayload};

#[test]
fn run_event_v1_has_identity_trace_session_and_timing() {
    let event = RunEvent::run_started("run_1", "trace_1", "assistant", "hello")
        .with_session_id("session_1")
        .with_metadata("source", json!("test"));

    assert_eq!(event.version().as_str(), "v1");
    assert!(event.event_id().as_str().starts_with("evt_"));
    assert_eq!(event.run_id(), "run_1");
    assert_eq!(event.trace_id(), "trace_1");
    assert_eq!(event.session_id(), Some("session_1"));
    assert!(event.created_at_ms() > 0);
    assert_eq!(event.agent_name(), Some("assistant"));
    assert!(matches!(event.payload(), RunEventPayload::RunStarted { input } if input == "hello"));

    let encoded = serde_json::to_value(&event).expect("serialize");
    assert_eq!(encoded["version"], "v1");
    assert_eq!(encoded["type"], "run_started");
    assert_eq!(encoded["run_id"], "run_1");
    assert_eq!(encoded["trace_id"], "trace_1");
    assert_eq!(encoded["session_id"], "session_1");
    assert_eq!(encoded["input"], "hello");

    let decoded: RunEvent = serde_json::from_value(encoded).expect("deserialize");
    assert_eq!(decoded.run_id(), "run_1");
}

#[test]
fn child_event_records_parent_run_and_parent_event() {
    let event = RunEvent::tool_call_started(
        "run_child",
        "trace_1",
        "researcher",
        2,
        "call_1",
        "read_file",
        json!({"path": "README.md"}),
    )
    .with_parent_run_id("run_parent")
    .with_parent_event_id("evt_parent");

    assert_eq!(event.parent_run_id(), Some("run_parent"));
    assert_eq!(event.parent_event_id(), Some("evt_parent"));
    assert_eq!(event.cycle_index(), Some(2));
}

#[test]
fn run_completed_payload_round_trips_status() {
    let event = RunEvent::run_completed("run_1", "trace_1", "assistant", AgentStatus::Completed);
    let value = serde_json::to_value(&event).expect("serialize");
    assert_eq!(value["type"], "run_completed");
    let decoded: RunEvent = serde_json::from_value(value).expect("deserialize");
    assert!(matches!(
        decoded.payload(),
        RunEventPayload::RunCompleted { status } if *status == AgentStatus::Completed
    ));
}
