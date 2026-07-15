use serde_json::json;
use sha2::{Digest, Sha256};
use vv_agent::events::ApprovalAction;
use vv_agent::{AgentErrorPayload, AgentStatus, RunEvent, RunEventPayload};

const PARITY_FIXTURE: &str = include_str!("fixtures/parity/run_events_v1.jsonl");
const PARITY_FIXTURE_SHA256: &str =
    "7d0d80a2587f242c2bdc04afc7452a632fab781845f2ea9a63742d2c62a0174e";

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

#[test]
fn run_events_v1_parity_fixture_has_stable_bytes_and_round_trips() {
    assert_eq!(
        format!("{:x}", Sha256::digest(PARITY_FIXTURE.as_bytes())),
        PARITY_FIXTURE_SHA256
    );

    let expected_types = [
        "run_started",
        "agent_started",
        "cycle_started",
        "llm_started",
        "assistant_delta",
        "tool_call_started",
        "tool_call_completed",
        "approval_requested",
        "approval_resolved",
        "memory_compact_started",
        "memory_compact_completed",
        "sub_run_started",
        "sub_run_completed",
        "handoff",
        "handoff_started",
        "handoff_completed",
        "session_persisted",
        "run_state_changed",
        "run_completed",
        "run_failed",
        "run_cancelled",
    ];
    let mut actual_types = Vec::new();

    for line in PARITY_FIXTURE.lines() {
        let expected: serde_json::Value = serde_json::from_str(line).expect("fixture JSON");
        let event: RunEvent = serde_json::from_str(line).expect("deserialize fixture event");
        let encoded = serde_json::to_value(&event).expect("serialize fixture event");

        actual_types.push(expected["type"].as_str().expect("event type").to_string());
        assert_eq!(event.event_id().as_str(), "evt_parity");
        assert_eq!(event.run_id(), "run_parity");
        assert_eq!(event.trace_id(), "trace_parity");
        assert_eq!(event.created_at(), 123.456789);
        assert_eq!(encoded, expected);
    }

    assert_eq!(actual_types, expected_types);
}

#[test]
fn approval_resolved_action_is_canonical_and_derives_approved() {
    let cases = [
        ("allow", ApprovalAction::Allow, true),
        ("allow_session", ApprovalAction::AllowSession, true),
        ("deny", ApprovalAction::Deny, false),
        ("timeout", ApprovalAction::Timeout, false),
    ];

    for (wire_action, expected_action, expected_approved) in cases {
        let event: RunEvent = serde_json::from_value(json!({
            "version": "v1",
            "type": "approval_resolved",
            "event_id": "evt_approval",
            "run_id": "run_approval",
            "trace_id": "trace_approval",
            "created_at": 123.456789,
            "request_id": "req_1",
            "tool_name": "shell",
            "tool_call_id": "call_1",
            "action": wire_action
        }))
        .expect("deserialize canonical approval action");

        assert_eq!(event.approval_action(), Some(expected_action));
        assert!(matches!(
            event.payload(),
            RunEventPayload::ApprovalResolved { approved, .. }
                if *approved == expected_approved
        ));
        let encoded = serde_json::to_value(&event).expect("serialize approval action");
        assert_eq!(encoded["action"], wire_action);
        assert_eq!(encoded["approved"], expected_approved);
        let restored: RunEvent = serde_json::from_value(encoded).expect("round-trip approval");
        assert_eq!(restored.approval_action(), Some(expected_action));
    }
}

#[test]
fn approval_resolved_rejects_conflicting_action_and_approved() {
    let error = serde_json::from_value::<RunEvent>(json!({
        "version": "v1",
        "type": "approval_resolved",
        "event_id": "evt_approval",
        "run_id": "run_approval",
        "trace_id": "trace_approval",
        "created_at": 123.456789,
        "request_id": "req_1",
        "tool_name": "shell",
        "tool_call_id": "call_1",
        "action": "timeout",
        "approved": true
    }))
    .expect_err("conflicting approval payload must fail");

    assert!(error.to_string().contains("conflicts with approved=true"));
}

#[test]
fn created_at_keeps_microseconds_and_reads_legacy_milliseconds() {
    let event: RunEvent = serde_json::from_value(json!({
        "version": "v1",
        "type": "run_started",
        "event_id": "evt_legacy",
        "run_id": "run_legacy",
        "trace_id": "trace_legacy",
        "created_at_ms": 123456.789,
        "input": "hello"
    }))
    .expect("deserialize legacy timestamp");

    assert_eq!(event.created_at(), 123.456789);
    assert_eq!(event.created_at_ms(), 123457);
    let encoded = serde_json::to_value(event).expect("serialize seconds timestamp");
    assert_eq!(encoded["created_at"], json!(123.456789));
    assert!(encoded.get("created_at_ms").is_none());
}

#[test]
fn empty_metadata_and_none_common_fields_are_omitted() {
    let encoded = serde_json::to_value(RunEvent::run_started(
        "run_compact",
        "trace_compact",
        "assistant",
        "hello",
    ))
    .expect("serialize compact event");

    assert!(encoded.get("metadata").is_none());
    assert!(encoded.get("session_id").is_none());
    assert!(encoded.get("cycle_index").is_none());
    assert!(encoded.get("parent_run_id").is_none());
}

#[test]
fn approval_preview_input_serializes_as_message() {
    let event: RunEvent = serde_json::from_value(json!({
        "version": "v1",
        "type": "approval_requested",
        "event_id": "evt_approval",
        "run_id": "run_approval",
        "trace_id": "trace_approval",
        "created_at": 123.456789,
        "request_id": "req_1",
        "tool_call_id": "call_1",
        "tool_name": "shell",
        "preview": "Allow command?"
    }))
    .expect("deserialize preview compatibility field");

    let encoded = serde_json::to_value(event).expect("serialize approval");
    assert_eq!(encoded["message"], "Allow command?");
    assert!(encoded.get("preview").is_none());
}

#[test]
fn run_failed_uses_string_wire_error_and_retains_typed_code_in_metadata() {
    let event = RunEvent::run_failed(
        "run_failed",
        "trace_failed",
        "assistant",
        AgentErrorPayload {
            message: "provider unavailable".to_string(),
            code: Some("provider_error".to_string()),
        },
    );

    let encoded = serde_json::to_value(event).expect("serialize failure");
    assert_eq!(encoded["error"], "provider unavailable");
    assert_eq!(encoded["metadata"]["error_code"], "provider_error");
}
