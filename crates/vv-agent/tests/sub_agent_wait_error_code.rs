use std::collections::BTreeMap;

use serde_json::{json, Value};
use vv_agent::{AgentStatus, CompletionReason, RunEvent, SubTaskManager, SubTaskOutcome};

const MANAGER_CONTRACT: &str = include_str!("fixtures/parity/manager_tool_envelope_v1.json");

fn wait_contract() -> Value {
    serde_json::from_str::<Value>(MANAGER_CONTRACT).expect("manager contract")["sync_wait_outcome"]
        .clone()
}

#[test]
fn manager_wait_outcome_stays_non_error_and_preserves_completion_observation() {
    let contract = wait_contract();
    let manager = SubTaskManager::default();
    let outcome = SubTaskOutcome {
        task_id: "wait-child".to_string(),
        agent_name: "researcher".to_string(),
        status: AgentStatus::WaitUser,
        session_id: Some("wait-session".to_string()),
        final_answer: None,
        wait_reason: Some("Approve dangerous.".to_string()),
        error: None,
        error_code: None,
        completion_reason: Some(CompletionReason::WaitUser),
        completion_tool_name: Some("dangerous".to_string()),
        partial_output: Some("proposed change".to_string()),
        cycles: 1,
        todo_list: Vec::new(),
        resolved: BTreeMap::new(),
    };
    assert_eq!(outcome.error_code, None);
    assert_eq!(contract["internal_error_code"], Value::Null);
    let outcome_wire = serde_json::to_value(&outcome).expect("waiting outcome wire");
    assert_eq!(contract["manager_status_error_code_field"], "omitted");
    assert!(!outcome_wire
        .as_object()
        .expect("outcome object")
        .contains_key("error_code"));
    manager.record_outcome("wait-child", outcome);

    let snapshot = manager.get("wait-child").expect("manager snapshot");
    let outcome = snapshot.outcome.expect("manager outcome");
    assert_eq!(outcome.status, AgentStatus::WaitUser);
    assert_eq!(outcome.error_code, None);
    assert_eq!(outcome.completion_reason, Some(CompletionReason::WaitUser));
    assert_eq!(outcome.completion_tool_name.as_deref(), Some("dangerous"));
    assert_eq!(outcome.partial_output.as_deref(), Some("proposed change"));

    let entry = manager
        .status_entries(&["wait-child".to_string()], "basic", 20)
        .pop()
        .expect("manager status entry");
    assert_eq!(entry["status"], "wait_user");
    assert!(entry.get("error_code").is_none());
    assert_eq!(entry["completion_reason"], "wait_user");
    assert_eq!(entry["completion_tool_name"], "dangerous");
    assert_eq!(entry["partial_output"], "proposed change");

    let sub_run: RunEvent = serde_json::from_value(json!({
        "version": "v1",
        "type": "sub_run_completed",
        "event_id": "evt_wait_child",
        "run_id": "run_wait_child",
        "trace_id": "trace_wait_child",
        "created_at": 1.0,
        "parent_tool_call_id": "parent_tool",
        "status": "wait_user",
        "wait_reason": "Approve dangerous.",
        "completion_reason": "wait_user",
        "metadata": {"cycles": 1}
    }))
    .expect("waiting sub-run event");
    let sub_run_wire = serde_json::to_value(sub_run).expect("sub-run wire");
    assert_eq!(contract["sub_run_event_error_code_field"], "omitted");
    assert!(!sub_run_wire["metadata"]
        .as_object()
        .expect("sub-run metadata")
        .contains_key("error_code"));
}
