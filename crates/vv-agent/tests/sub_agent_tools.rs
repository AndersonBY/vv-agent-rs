use std::collections::BTreeMap;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex, MutexGuard, OnceLock,
};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};
use vv_agent::{
    build_default_registry, register_sub_agent_session, sub_agent_session_registry,
    unregister_sub_agent_session, AgentStatus, Message, RuntimeExecutionBackend, SubAgentSession,
    SubAgentSessionListener, SubTaskManager, SubTaskOutcome, ThreadBackend, ToolCall, ToolContext,
    ToolResultStatus,
};

struct SubAgentRegistryTestLock {
    _guard: MutexGuard<'static, ()>,
}

impl Drop for SubAgentRegistryTestLock {
    fn drop(&mut self) {
        sub_agent_session_registry().clear();
    }
}

fn isolated_sub_agent_registry() -> SubAgentRegistryTestLock {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let guard = LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("sub-agent registry test lock");
    sub_agent_session_registry().clear();
    SubAgentRegistryTestLock { _guard: guard }
}

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
fn create_sub_task_batch_uses_execution_backend_parallel_map() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    context.execution_backend = Some(RuntimeExecutionBackend::Thread(ThreadBackend::new(2)));

    let current_concurrent = Arc::new(AtomicUsize::new(0));
    let max_concurrent = Arc::new(AtomicUsize::new(0));
    let observed_max = Arc::clone(&max_concurrent);
    context.sub_task_runner = Some(Arc::new(move |request| {
        let active = current_concurrent.fetch_add(1, Ordering::SeqCst) + 1;
        max_concurrent.fetch_max(active, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(80));
        current_concurrent.fetch_sub(1, Ordering::SeqCst);
        SubTaskOutcome {
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
        }
    }));

    let result = registry
        .execute(
            &ToolCall::new(
                "sub_batch_parallel",
                "create_sub_task",
                BTreeMap::from([
                    ("agent_id".to_string(), json!("writer-sub")),
                    (
                        "tasks".to_string(),
                        json!([
                            {"task_description": "Write section A"},
                            {"task_description": "Write section B"},
                            {"task_description": "Write section C"}
                        ]),
                    ),
                ]),
            ),
            &mut context,
        )
        .expect("create_sub_task");

    assert_eq!(result.status, ToolResultStatus::Success);
    assert!(
        observed_max.load(Ordering::SeqCst) >= 2,
        "expected batch sub-tasks to run through execution_backend.parallel_map"
    );
}

#[test]
fn create_sub_task_coerces_python_style_boolean_arguments() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    context.task_id = "parent".to_string();
    let manager = SubTaskManager::default();
    context.sub_task_manager = Some(manager.clone());
    let captured_flags = Arc::new(Mutex::new(Vec::new()));
    let flags = Arc::clone(&captured_flags);
    context.sub_task_runner = Some(Arc::new(move |request| {
        flags
            .lock()
            .expect("captured flags")
            .push(request.include_main_summary);
        SubTaskOutcome {
            task_id: "sub_task".to_string(),
            agent_name: request.agent_name,
            status: AgentStatus::Completed,
            session_id: None,
            final_answer: Some("done".to_string()),
            wait_reason: None,
            error: None,
            cycles: 1,
            todo_list: Vec::new(),
            resolved: BTreeMap::new(),
        }
    }));

    let result = registry
        .execute(
            &ToolCall::new(
                "sub_bool",
                "create_sub_task",
                BTreeMap::from([
                    ("agent_id".to_string(), json!("writer-sub")),
                    ("task_description".to_string(), json!("Write summary")),
                    ("include_main_summary".to_string(), json!("true")),
                    ("wait_for_completion".to_string(), json!("0")),
                ]),
            ),
            &mut context,
        )
        .expect("create_sub_task");

    assert_eq!(result.status, ToolResultStatus::Success);
    let payload: Value = serde_json::from_str(&result.content).expect("payload");
    assert_eq!(payload["wait_for_completion"], false);
    assert_eq!(payload["status"], "running");
    let task_id = payload["task_id"].as_str().expect("task id");
    manager.wait(task_id, None);
    assert_eq!(
        captured_flags.lock().expect("captured flags").as_slice(),
        &[true]
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
    let _registry_lock = isolated_sub_agent_registry();
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

#[test]
fn sub_task_status_can_continue_completed_registered_session() {
    let _registry_lock = isolated_sub_agent_registry();
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let manager = SubTaskManager::default();
    let mut context = ToolContext::new(workspace.path());
    context.sub_task_manager = Some(manager.clone());
    let continued = Arc::new(Mutex::new(Vec::<String>::new()));
    let session = Arc::new(ContinuingSubAgentSession {
        continued: Arc::clone(&continued),
    });
    register_sub_agent_session("sub-session-continued", session.clone());
    manager.attach_session(
        "sub-task-completed",
        "sub-session-continued",
        "researcher",
        "initial task",
        context.workspace_backend.clone(),
        session,
    );
    manager.record_outcome(
        "sub-task-completed",
        SubTaskOutcome {
            task_id: "sub-task-completed".to_string(),
            agent_name: "researcher".to_string(),
            status: AgentStatus::Completed,
            session_id: Some("sub-session-continued".to_string()),
            final_answer: Some("initial done".to_string()),
            wait_reason: None,
            error: None,
            cycles: 1,
            todo_list: Vec::new(),
            resolved: BTreeMap::new(),
        },
    );

    let result = registry
        .execute(
            &ToolCall::new(
                "sub_status_continue",
                "sub_task_status",
                BTreeMap::from([
                    ("task_ids".to_string(), json!(["sub-task-completed"])),
                    ("message".to_string(), json!("add appendix")),
                    ("wait_for_response".to_string(), json!("yes")),
                    ("detail_level".to_string(), json!("snapshot")),
                ]),
            ),
            &mut context,
        )
        .expect("sub_task_status continue");

    unregister_sub_agent_session("sub-session-continued");
    assert_eq!(result.status, ToolResultStatus::Success);
    assert_eq!(
        continued.lock().expect("continued").as_slice(),
        ["add appendix"]
    );
    let payload: Value = serde_json::from_str(&result.content).expect("payload");
    assert_eq!(payload["interaction"]["task_id"], "sub-task-completed");
    assert_eq!(payload["interaction"]["action"], "continued");
    assert_eq!(payload["tasks"][0]["status"], "completed");
    assert_eq!(payload["tasks"][0]["final_answer"], "continued done");
    assert_eq!(
        payload["tasks"][0]["snapshot"]["recent_activity"],
        "continued done"
    );
}

#[test]
fn sub_task_status_can_continue_completed_attached_session_without_global_registration() {
    let _registry_lock = isolated_sub_agent_registry();
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let manager = SubTaskManager::default();
    let mut context = ToolContext::new(workspace.path());
    context.sub_task_manager = Some(manager.clone());
    let continued = Arc::new(Mutex::new(Vec::<String>::new()));
    let session = Arc::new(ContinuingSubAgentSession {
        continued: Arc::clone(&continued),
    });
    manager.attach_session(
        "sub-task-attached",
        "sub-session-attached",
        "researcher",
        "initial task",
        context.workspace_backend.clone(),
        session,
    );
    manager.record_outcome(
        "sub-task-attached",
        SubTaskOutcome {
            task_id: "sub-task-attached".to_string(),
            agent_name: "researcher".to_string(),
            status: AgentStatus::Completed,
            session_id: Some("sub-session-attached".to_string()),
            final_answer: Some("initial done".to_string()),
            wait_reason: None,
            error: None,
            cycles: 1,
            todo_list: Vec::new(),
            resolved: BTreeMap::new(),
        },
    );
    sub_agent_session_registry().clear();

    let result = registry
        .execute(
            &ToolCall::new(
                "sub_status_continue_attached",
                "sub_task_status",
                BTreeMap::from([
                    ("task_ids".to_string(), json!(["sub-task-attached"])),
                    ("message".to_string(), json!("add appendix")),
                    ("wait_for_response".to_string(), json!(true)),
                    ("detail_level".to_string(), json!("snapshot")),
                ]),
            ),
            &mut context,
        )
        .expect("sub_task_status continue");

    assert_eq!(result.status, ToolResultStatus::Success);
    assert_eq!(
        continued.lock().expect("continued").as_slice(),
        ["add appendix"]
    );
    let payload: Value = serde_json::from_str(&result.content).expect("payload");
    assert_eq!(payload["interaction"]["action"], "continued");
    assert_eq!(payload["tasks"][0]["status"], "completed");
    assert_eq!(payload["tasks"][0]["final_answer"], "continued done");
    assert!(sub_agent_session_registry()
        .get("sub-session-attached")
        .is_none());
}

#[test]
fn sub_task_manager_sanitizes_session_messages_before_continue_like_python() {
    let _registry_lock = isolated_sub_agent_registry();
    let manager = SubTaskManager::default();
    let workspace = tempfile::tempdir().expect("workspace");
    let messages = Arc::new(Mutex::new(vec![
        Message::system("sys"),
        {
            let mut message = Message::assistant("");
            message.reasoning_content = Some("thinking only".to_string());
            message
        },
        {
            let mut message = Message::assistant("");
            message.tool_calls = vec![ToolCall::new(
                "tool-1",
                "read_file",
                BTreeMap::from([("path".to_string(), json!("README.md"))]),
            )];
            message
        },
    ]));
    let snapshot = Arc::new(Mutex::new(Vec::<Message>::new()));
    let session = Arc::new(SanitizingSubAgentSession {
        messages: Arc::clone(&messages),
        snapshot: Arc::clone(&snapshot),
    });
    manager.attach_session(
        "sub-sanitize",
        "sub-session-sanitize",
        "researcher",
        "initial task",
        Arc::new(vv_agent::workspace::LocalWorkspaceBackend::new(
            workspace.path(),
        )),
        session,
    );
    manager.record_outcome(
        "sub-sanitize",
        SubTaskOutcome {
            task_id: "sub-sanitize".to_string(),
            agent_name: "researcher".to_string(),
            status: AgentStatus::Completed,
            session_id: Some("sub-session-sanitize".to_string()),
            final_answer: Some("initial done".to_string()),
            wait_reason: None,
            error: None,
            cycles: 1,
            todo_list: Vec::new(),
            resolved: BTreeMap::new(),
        },
    );

    manager
        .continue_task("sub-sanitize", "resume")
        .expect("continue sub task");
    assert!(manager.wait("sub-sanitize", Some(Duration::from_secs(5))));

    assert_eq!(
        snapshot.lock().expect("snapshot").as_slice(),
        &[Message::system("sys")]
    );
    assert_eq!(
        messages.lock().expect("messages").as_slice(),
        &[Message::assistant("continued done")]
    );
}

#[test]
fn sub_task_status_rejects_max_cycles_continuation() {
    let _registry_lock = isolated_sub_agent_registry();
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let manager = SubTaskManager::default();
    let mut context = ToolContext::new(workspace.path());
    context.sub_task_manager = Some(manager.clone());
    manager.record_outcome(
        "sub-task-max-cycles",
        SubTaskOutcome {
            task_id: "sub-task-max-cycles".to_string(),
            agent_name: "researcher".to_string(),
            status: AgentStatus::MaxCycles,
            session_id: Some("sub-session-max-cycles".to_string()),
            final_answer: None,
            wait_reason: None,
            error: Some("max cycles".to_string()),
            cycles: 8,
            todo_list: Vec::new(),
            resolved: BTreeMap::new(),
        },
    );

    let result = registry
        .execute(
            &ToolCall::new(
                "sub_status_max_cycles",
                "sub_task_status",
                BTreeMap::from([
                    ("task_ids".to_string(), json!(["sub-task-max-cycles"])),
                    ("message".to_string(), json!("try again")),
                ]),
            ),
            &mut context,
        )
        .expect("sub_task_status max cycles");

    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(
        result.error_code.as_deref(),
        Some("sub_task_max_cycles_reached")
    );
}

#[test]
fn sub_task_status_snapshot_tracks_session_activity_and_workspace_files() {
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::write(workspace.path().join("notes.md"), "# Notes\n").expect("notes");
    std::fs::create_dir(workspace.path().join(".internal")).expect("internal dir");
    std::fs::write(workspace.path().join(".internal/secret.txt"), "secret").expect("secret");
    let registry = build_default_registry();
    let manager = SubTaskManager::default();
    let mut context = ToolContext::new(workspace.path());
    context.sub_task_manager = Some(manager.clone());
    let session = Arc::new(EventingSubAgentSession::default());
    manager.submit(
        "sub-task-snapshot",
        "sub-session-snapshot",
        "researcher",
        "Inspect docs",
        || {
            thread::sleep(Duration::from_millis(100));
            SubTaskOutcome {
                task_id: "sub-task-snapshot".to_string(),
                agent_name: "researcher".to_string(),
                status: AgentStatus::Completed,
                session_id: Some("sub-session-snapshot".to_string()),
                final_answer: Some("done".to_string()),
                wait_reason: None,
                error: None,
                cycles: 1,
                todo_list: Vec::new(),
                resolved: BTreeMap::new(),
            }
        },
    );
    manager.attach_session(
        "sub-task-snapshot",
        "sub-session-snapshot",
        "researcher",
        "Inspect docs",
        context.workspace_backend.clone(),
        session.clone(),
    );
    session.emit(
        "session_run_start",
        BTreeMap::from([("prompt".to_string(), json!("Inspect docs"))]),
    );
    session.emit(
        "cycle_started",
        BTreeMap::from([("cycle".to_string(), json!(1))]),
    );
    session.emit(
        "cycle_llm_response",
        BTreeMap::from([
            ("cycle".to_string(), json!(1)),
            (
                "assistant_preview".to_string(),
                json!("Reading the workspace files"),
            ),
        ]),
    );
    session.emit(
        "tool_result",
        BTreeMap::from([
            ("tool_name".to_string(), json!("read_file")),
            ("tool_call_id".to_string(), json!("tool-1")),
            ("status".to_string(), json!("SUCCESS")),
        ]),
    );

    let result = registry
        .execute(
            &ToolCall::new(
                "sub_status_snapshot",
                "sub_task_status",
                BTreeMap::from([
                    ("task_ids".to_string(), json!(["sub-task-snapshot"])),
                    ("detail_level".to_string(), json!("snapshot")),
                ]),
            ),
            &mut context,
        )
        .expect("sub_task_status snapshot");

    assert_eq!(result.status, ToolResultStatus::Success);
    let payload: Value = serde_json::from_str(&result.content).expect("payload");
    let task = &payload["tasks"][0];
    assert_eq!(task["status"], "running");
    assert_eq!(
        task["snapshot"]["recent_activity"],
        "Reading the workspace files"
    );
    assert_eq!(task["snapshot"]["latest_tool_call"]["name"], "read_file");
    assert_eq!(task["snapshot"]["latest_cycle"]["cycle_index"], 1);
    assert_eq!(task["snapshot"]["workspace_files"], json!(["notes.md"]));
    assert_eq!(task["snapshot"]["workspace_file_count"], 1);
    assert_eq!(task["snapshot"]["workspace_files_truncated"], false);
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

struct ContinuingSubAgentSession {
    continued: Arc<Mutex<Vec<String>>>,
}

impl SubAgentSession for ContinuingSubAgentSession {
    fn steer(&self, prompt: &str) -> Result<(), String> {
        self.continue_run(prompt).map(|_| ())
    }

    fn continue_run(&self, prompt: &str) -> Result<SubTaskOutcome, String> {
        self.continued
            .lock()
            .expect("continued")
            .push(prompt.to_string());
        thread::sleep(Duration::from_millis(25));
        Ok(SubTaskOutcome {
            task_id: "sub-task-completed".to_string(),
            agent_name: "researcher".to_string(),
            status: AgentStatus::Completed,
            session_id: Some("sub-session-continued".to_string()),
            final_answer: Some("continued done".to_string()),
            wait_reason: None,
            error: None,
            cycles: 2,
            todo_list: Vec::new(),
            resolved: BTreeMap::new(),
        })
    }
}

struct SanitizingSubAgentSession {
    messages: Arc<Mutex<Vec<Message>>>,
    snapshot: Arc<Mutex<Vec<Message>>>,
}

impl SubAgentSession for SanitizingSubAgentSession {
    fn steer(&self, prompt: &str) -> Result<(), String> {
        self.continue_run(prompt).map(|_| ())
    }

    fn sanitize_for_resume(&self) -> usize {
        let mut messages = self.messages.lock().expect("messages");
        let sanitized = vv_agent::memory::sanitize_for_resume(&messages);
        let removed = messages.len().saturating_sub(sanitized.len());
        *messages = sanitized;
        removed
    }

    fn continue_run(&self, _prompt: &str) -> Result<SubTaskOutcome, String> {
        let mut messages = self.messages.lock().expect("messages");
        *self.snapshot.lock().expect("snapshot") = messages.clone();
        *messages = vec![Message::assistant("continued done")];
        Ok(SubTaskOutcome {
            task_id: "sub-sanitize".to_string(),
            agent_name: "researcher".to_string(),
            status: AgentStatus::Completed,
            session_id: Some("sub-session-sanitize".to_string()),
            final_answer: Some("continued done".to_string()),
            wait_reason: None,
            error: None,
            cycles: 2,
            todo_list: Vec::new(),
            resolved: BTreeMap::new(),
        })
    }
}

#[derive(Default)]
struct EventingSubAgentSession {
    listeners: Mutex<Vec<SubAgentSessionListener>>,
}

impl EventingSubAgentSession {
    fn emit(&self, event: &str, payload: BTreeMap<String, Value>) {
        let listeners = self.listeners.lock().expect("listeners").clone();
        for listener in listeners {
            listener(event, &payload);
        }
    }
}

impl SubAgentSession for EventingSubAgentSession {
    fn steer(&self, _prompt: &str) -> Result<(), String> {
        Ok(())
    }

    fn subscribe(
        &self,
        listener: SubAgentSessionListener,
    ) -> Option<vv_agent::SubAgentSessionUnsubscribe> {
        self.listeners.lock().expect("listeners").push(listener);
        Some(Box::new(|| {}))
    }
}
