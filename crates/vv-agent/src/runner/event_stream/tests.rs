use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::{map_runtime_event, map_stream_event, RuntimeEventContext};
use crate::events::{ApprovalAction, RunEventPayload};

fn event_context() -> RuntimeEventContext {
    RuntimeEventContext::new(
        "run_context",
        "trace_context",
        "context-agent",
        Some("session_context".to_string()),
        "context input",
    )
}

fn runtime_payload() -> BTreeMap<String, Value> {
    BTreeMap::from([
        ("task_id".to_string(), json!("run_runtime")),
        ("trace_id".to_string(), json!("trace_runtime")),
        ("agent_name".to_string(), json!("assistant")),
        ("session_id".to_string(), json!("session_payload")),
        ("cycle".to_string(), json!(3)),
        ("model".to_string(), json!("model-parity")),
    ])
}

#[test]
fn maps_agent_and_cycle_while_rejecting_superseded_llm_started() {
    let mut payload = runtime_payload();
    payload.insert("producer_extra".to_string(), json!({"nested": true}));
    let context = event_context();
    let agent = map_runtime_event("agent_started", &payload, &context).expect("agent event");
    let cycle = map_runtime_event("cycle_started", &payload, &context).expect("cycle event");
    let llm = map_runtime_event("llm_started", &payload, &context);

    assert!(matches!(agent.payload(), RunEventPayload::AgentStarted));
    assert!(matches!(cycle.payload(), RunEventPayload::CycleStarted));
    assert!(llm.is_none());
    assert_eq!(agent.run_id(), "run_context");
    assert_eq!(agent.trace_id(), "trace_context");
    assert_eq!(agent.agent_name(), Some("context-agent"));
    assert_eq!(agent.session_id(), Some("session_context"));
    assert_eq!(cycle.session_id(), Some("session_context"));
    assert_eq!(
        agent.metadata().get("producer_extra"),
        Some(&json!({"nested": true}))
    );
    assert_eq!(agent.metadata().get("task_id"), Some(&json!("run_runtime")));
}

#[test]
fn maps_tool_arguments_and_run_final_output_to_top_level_wire_fields() {
    let context = event_context();
    let mut tool_payload = runtime_payload();
    tool_payload.insert("tool_call_id".to_string(), json!("call_1"));
    tool_payload.insert("tool_name".to_string(), json!("search"));
    tool_payload.insert("tool_arguments".to_string(), json!({"query": "parity"}));
    let tool = map_runtime_event("tool_call_started", &tool_payload, &context).expect("tool event");
    let tool_wire = serde_json::to_value(tool).expect("serialize tool event");
    assert_eq!(tool_wire["arguments"], json!({"query": "parity"}));

    let mut completed_payload = runtime_payload();
    completed_payload.insert("final_answer".to_string(), json!("done"));
    let completed =
        map_runtime_event("run_completed", &completed_payload, &context).expect("run event");
    let completed_wire = serde_json::to_value(completed).expect("serialize run event");
    assert_eq!(completed_wire["final_output"], "done");
}

#[test]
fn maps_only_real_stream_delta_and_does_not_relabel_full_cycle_message() {
    let context = event_context();
    let stream_payload = BTreeMap::from([
        ("event".to_string(), json!("assistant_delta")),
        ("cycle".to_string(), json!(4)),
        ("content_delta".to_string(), json!("token")),
    ]);
    let event = map_stream_event(&stream_payload, &context).expect("stream delta");
    assert!(matches!(
        event.payload(),
        RunEventPayload::AssistantDelta { delta, .. } if delta == "token"
    ));
    assert!(event.metadata().is_empty());

    let full_message = BTreeMap::from([
        ("cycle".to_string(), json!(4)),
        ("assistant_message".to_string(), json!("complete answer")),
    ]);
    assert!(map_runtime_event("cycle_llm_response", &full_message, &context).is_none());
}

#[test]
fn maps_approval_action_without_collapsing_session_or_timeout_decisions() {
    let context = event_context();
    let cases = [
        ("allow", ApprovalAction::Allow),
        ("allow_session", ApprovalAction::AllowSession),
        ("deny", ApprovalAction::Deny),
        ("timeout", ApprovalAction::Timeout),
    ];

    for (action, expected_action) in cases {
        let payload = BTreeMap::from([
            ("request_id".to_string(), json!("request_1")),
            ("tool_call_id".to_string(), json!("call_1")),
            ("tool_name".to_string(), json!("shell")),
            ("action".to_string(), json!(action)),
        ]);
        let event =
            map_runtime_event("approval_resolved", &payload, &context).expect("approval event");

        assert_eq!(event.approval_action(), Some(expected_action));
        assert!(matches!(
            event.payload(),
            RunEventPayload::ApprovalResolved { action, .. }
                if *action == expected_action
        ));
        let encoded = serde_json::to_value(event).expect("approval wire payload");
        assert_eq!(encoded["action"], action);
        assert!(encoded.get("approved").is_none());
    }
}
