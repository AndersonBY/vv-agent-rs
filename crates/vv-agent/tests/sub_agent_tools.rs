use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};
use vv_agent::{
    build_default_registry, register_sub_agent_session, sub_agent_session_registry,
    unregister_sub_agent_session, AgentStatus, SubAgentSession, SubTaskManager, SubTaskOutcome,
    ToolCall, ToolContext, ToolResultStatus,
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

#[test]
fn create_sub_task_can_start_async_task_and_query_status() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    context.task_id = "parent".to_string();
    context.sub_task_manager = Some(SubTaskManager::default());
    context.sub_task_runner = Some(Arc::new(|request| {
        thread::sleep(Duration::from_millis(50));
        SubTaskOutcome {
            task_id: request
                .metadata
                .get("task_id")
                .and_then(Value::as_str)
                .unwrap_or("missing-task-id")
                .to_string(),
            agent_name: request.agent_name,
            status: AgentStatus::Completed,
            session_id: request
                .metadata
                .get("session_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            final_answer: Some(format!("done: {}", request.task_description)),
            wait_reason: None,
            error: None,
            cycles: 2,
            todo_list: Vec::new(),
            resolved: BTreeMap::from([("backend".to_string(), "deepseek".to_string())]),
        }
    }));

    let start = registry
        .execute(
            &ToolCall::new(
                "sub_async",
                "create_sub_task",
                BTreeMap::from([
                    ("agent_id".to_string(), json!("research-sub")),
                    ("task_description".to_string(), json!("Collect async facts")),
                    ("wait_for_completion".to_string(), json!(false)),
                ]),
            ),
            &mut context,
        )
        .expect("create_sub_task async");

    assert_eq!(start.status, ToolResultStatus::Success);
    let start_payload: Value = serde_json::from_str(&start.content).expect("start payload");
    assert_eq!(start_payload["status"], "running");
    assert_eq!(start_payload["agent_name"], "research-sub");
    assert_eq!(start_payload["wait_for_completion"], false);
    let task_id = start_payload["task_id"]
        .as_str()
        .expect("task_id")
        .to_string();
    assert!(task_id.starts_with("parent_sub_research-sub_"));

    let mut final_status = None;
    for _ in 0..20 {
        let status = registry
            .execute(
                &ToolCall::new(
                    "sub_status",
                    "sub_task_status",
                    BTreeMap::from([
                        ("task_ids".to_string(), json!([task_id])),
                        ("detail_level".to_string(), json!("snapshot")),
                    ]),
                ),
                &mut context,
            )
            .expect("sub_task_status");
        assert_eq!(status.status, ToolResultStatus::Success);
        let payload: Value = serde_json::from_str(&status.content).expect("status payload");
        let task_status = payload["tasks"][0]["status"].as_str().unwrap_or_default();
        if task_status == "completed" {
            final_status = Some(payload);
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }

    let payload = final_status.expect("async sub-task completed");
    assert_eq!(payload["detail_level"], "snapshot");
    assert_eq!(payload["tasks"][0]["task_id"], task_id);
    assert_eq!(
        payload["tasks"][0]["final_answer"],
        "done: Collect async facts"
    );
    assert_eq!(payload["tasks"][0]["cycles"], 2);
    assert_eq!(payload["tasks"][0]["resolved"]["backend"], "deepseek");
    assert_eq!(
        payload["tasks"][0]["snapshot"]["task_title"],
        "Collect async facts"
    );
}

#[test]
fn sub_task_status_reports_missing_and_invalid_task_ids() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    context.sub_task_manager = Some(SubTaskManager::default());

    let invalid = registry
        .execute(
            &ToolCall::new("sub_status_invalid", "sub_task_status", BTreeMap::new()),
            &mut context,
        )
        .expect("sub_task_status invalid");
    assert_eq!(invalid.status, ToolResultStatus::Error);
    assert_eq!(invalid.error_code.as_deref(), Some("invalid_task_ids"));

    let missing = registry
        .execute(
            &ToolCall::new(
                "sub_status_missing",
                "sub_task_status",
                BTreeMap::from([("task_ids".to_string(), json!(["unknown"]))]),
            ),
            &mut context,
        )
        .expect("sub_task_status missing");
    assert_eq!(missing.status, ToolResultStatus::Success);
    let payload: Value = serde_json::from_str(&missing.content).expect("missing payload");
    assert_eq!(payload["tasks"][0]["status"], "missing");
    assert_eq!(payload["tasks"][0]["task_id"], "unknown");
}

#[test]
fn sub_task_status_can_steer_registered_running_session() {
    sub_agent_session_registry().clear();
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let manager = SubTaskManager::default();
    let mut context = ToolContext::new(workspace.path());
    context.sub_task_manager = Some(manager.clone());
    let received = Arc::new(Mutex::new(Vec::<String>::new()));
    register_sub_agent_session(
        "sub-session-1",
        Arc::new(RecordingSubAgentSession {
            received: Arc::clone(&received),
        }),
    );
    manager.submit(
        "sub-task-1",
        "sub-session-1",
        "researcher",
        "Collect facts",
        || {
            thread::sleep(Duration::from_millis(100));
            SubTaskOutcome {
                task_id: "sub-task-1".to_string(),
                agent_name: "researcher".to_string(),
                status: AgentStatus::Completed,
                session_id: Some("sub-session-1".to_string()),
                final_answer: Some("done".to_string()),
                wait_reason: None,
                error: None,
                cycles: 1,
                todo_list: Vec::new(),
                resolved: BTreeMap::new(),
            }
        },
    );

    let result = registry
        .execute(
            &ToolCall::new(
                "sub_status_message",
                "sub_task_status",
                BTreeMap::from([
                    ("task_ids".to_string(), json!(["sub-task-1"])),
                    ("message".to_string(), json!("focus github")),
                ]),
            ),
            &mut context,
        )
        .expect("sub_task_status message");

    unregister_sub_agent_session("sub-session-1");
    assert_eq!(result.status, ToolResultStatus::Success);
    assert_eq!(
        received.lock().expect("received").as_slice(),
        ["focus github"]
    );
    let payload: Value = serde_json::from_str(&result.content).expect("payload");
    assert_eq!(payload["interaction"]["task_id"], "sub-task-1");
    assert_eq!(payload["interaction"]["action"], "message_queued");
}

struct RecordingSubAgentSession {
    received: Arc<Mutex<Vec<String>>>,
}

impl SubAgentSession for RecordingSubAgentSession {
    fn steer(&self, prompt: &str) -> Result<(), String> {
        self.received
            .lock()
            .expect("received")
            .push(prompt.to_string());
        Ok(())
    }
}
