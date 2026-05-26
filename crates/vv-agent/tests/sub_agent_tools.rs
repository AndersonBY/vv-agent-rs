use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use vv_agent::{
    build_default_registry, AgentStatus, SubTaskOutcome, ToolCall, ToolContext, ToolResultStatus,
};

#[test]
fn create_sub_task_runs_injected_runner_for_single_task() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    let captured = Arc::new(Mutex::new(Vec::new()));
    let captured_for_runner = captured.clone();
    context.sub_task_runner = Some(Arc::new(move |request| {
        captured_for_runner
            .lock()
            .expect("captured")
            .push(request.clone());
        SubTaskOutcome {
            task_id: "sub_1".to_string(),
            agent_name: request.agent_name,
            status: AgentStatus::Completed,
            session_id: None,
            final_answer: Some("sub-result".to_string()),
            wait_reason: None,
            error: None,
            cycles: 1,
            todo_list: Vec::new(),
            resolved: BTreeMap::from([("backend".to_string(), "moonshot".to_string())]),
        }
    }));

    let result = registry
        .execute(
            &ToolCall::new(
                "sub_1",
                "create_sub_task",
                BTreeMap::from([
                    ("agent_id".to_string(), json!("research-sub")),
                    ("task_description".to_string(), json!("Collect core facts")),
                    (
                        "output_requirements".to_string(),
                        json!("Return short bullet list"),
                    ),
                ]),
            ),
            &mut context,
        )
        .expect("create_sub_task");

    assert_eq!(result.status, ToolResultStatus::Success);
    let payload: Value = serde_json::from_str(&result.content).expect("payload");
    assert_eq!(payload["status"], "completed");
    assert_eq!(payload["final_answer"], "sub-result");
    assert_eq!(payload["resolved"]["backend"], "moonshot");
    let captured = captured.lock().expect("captured");
    assert_eq!(captured[0].agent_name, "research-sub");
    assert_eq!(captured[0].task_description, "Collect core facts");
    assert_eq!(captured[0].output_requirements, "Return short bullet list");
}

#[test]
fn create_sub_task_batch_aggregates_results() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    context.sub_task_runner = Some(Arc::new(|request| SubTaskOutcome {
        task_id: format!("sub_{}", request.task_description.replace(' ', "_")),
        agent_name: request.agent_name,
        status: AgentStatus::Completed,
        session_id: None,
        final_answer: Some(format!("done: {}", request.task_description)),
        wait_reason: None,
        error: None,
        cycles: 1,
        todo_list: Vec::new(),
        resolved: BTreeMap::new(),
    }));

    let result = registry
        .execute(
            &ToolCall::new(
                "sub_batch",
                "create_sub_task",
                BTreeMap::from([
                    ("agent_id".to_string(), json!("writer-sub")),
                    (
                        "tasks".to_string(),
                        json!([
                            {"task_description": "Write section A"},
                            {"task_description": "Write section B"}
                        ]),
                    ),
                ]),
            ),
            &mut context,
        )
        .expect("create_sub_task");

    assert_eq!(result.status, ToolResultStatus::Success);
    let payload: Value = serde_json::from_str(&result.content).expect("payload");
    assert_eq!(payload["summary"]["total"], 2);
    assert_eq!(payload["summary"]["completed"], 2);
    assert_eq!(payload["summary"]["failed"], 0);
    assert_eq!(
        payload["results"][0]["final_answer"],
        "done: Write section A"
    );
    assert_eq!(
        payload["results"][1]["final_answer"],
        "done: Write section B"
    );
}

#[test]
fn create_sub_task_errors_when_runner_is_missing() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());

    let result = registry
        .execute(
            &ToolCall::new(
                "sub_missing",
                "create_sub_task",
                BTreeMap::from([
                    ("agent_id".to_string(), json!("research-sub")),
                    ("task_description".to_string(), json!("Collect facts")),
                ]),
            ),
            &mut context,
        )
        .expect("create_sub_task");

    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(result.error_code.as_deref(), Some("sub_agents_not_enabled"));
}
