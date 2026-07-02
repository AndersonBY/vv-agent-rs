use super::*;

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
fn sub_task_status_can_wait_for_background_task_completion() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let manager = SubTaskManager::default();
    let mut context = ToolContext::new(workspace.path());
    context.sub_task_manager = Some(manager.clone());
    manager
        .submit(
            "sub-wait",
            "session-wait",
            "researcher",
            "Wait for background completion",
            || {
                thread::sleep(Duration::from_millis(200));
                SubTaskOutcome {
                    task_id: "sub-wait".to_string(),
                    agent_name: "researcher".to_string(),
                    status: AgentStatus::Completed,
                    session_id: Some("session-wait".to_string()),
                    final_answer: Some("waited done".to_string()),
                    wait_reason: None,
                    error: None,
                    cycles: 1,
                    todo_list: Vec::new(),
                    resolved: BTreeMap::new(),
                }
            },
        )
        .expect("submit waiting sub-task");

    let started_at = std::time::Instant::now();
    let result = registry
        .execute(
            &ToolCall::new(
                "sub_status_wait",
                "sub_task_status",
                BTreeMap::from([
                    ("task_ids".to_string(), json!(["sub-wait"])),
                    ("wait_for_completion".to_string(), json!(true)),
                    ("check_interval_seconds".to_string(), json!(300)),
                    ("max_wait_seconds".to_string(), json!(3600)),
                ]),
            ),
            &mut context,
        )
        .expect("sub_task_status wait");

    assert!(started_at.elapsed() < Duration::from_secs(1));
    assert_eq!(result.status, ToolResultStatus::Success);
    let payload: Value = serde_json::from_str(&result.content).expect("payload");
    assert_eq!(payload["wait_for_completion"], true);
    assert_eq!(payload["wait_exceeded"], false);
    assert_eq!(payload["running_task_ids"], json!([]));
    assert_eq!(payload["suggested_next_check_after_seconds"], 300);
    assert_eq!(payload["tasks"][0]["status"], "completed");
    assert_eq!(payload["tasks"][0]["final_answer"], "waited done");
}

#[test]
fn sub_task_manager_rejects_duplicate_running_submit() {
    let manager = SubTaskManager::default();
    manager
        .submit("sub-dup", "session-dup", "researcher", "first run", || {
            thread::sleep(Duration::from_millis(100));
            SubTaskOutcome {
                task_id: "sub-dup".to_string(),
                agent_name: "researcher".to_string(),
                status: AgentStatus::Completed,
                session_id: Some("session-dup".to_string()),
                final_answer: Some("done".to_string()),
                wait_reason: None,
                error: None,
                cycles: 1,
                todo_list: Vec::new(),
                resolved: BTreeMap::new(),
            }
        })
        .expect("first submit");

    let error = manager
        .submit(
            "sub-dup",
            "session-dup-2",
            "researcher",
            "second run",
            || SubTaskOutcome {
                task_id: "sub-dup".to_string(),
                agent_name: "researcher".to_string(),
                status: AgentStatus::Completed,
                session_id: Some("session-dup-2".to_string()),
                final_answer: Some("should not run".to_string()),
                wait_reason: None,
                error: None,
                cycles: 1,
                todo_list: Vec::new(),
                resolved: BTreeMap::new(),
            },
        )
        .expect_err("duplicate running submit should fail");

    assert!(error.contains("already running"));
    assert!(manager.wait("sub-dup", Some(Duration::from_secs(5))));
}

#[test]
fn sub_task_manager_get_and_wait_return_agent_record_snapshot() {
    let manager = SubTaskManager::default();
    manager
        .submit(
            "sub-record",
            "session-record",
            "researcher",
            "first run",
            || {
                thread::sleep(Duration::from_millis(50));
                SubTaskOutcome {
                    task_id: "sub-record".to_string(),
                    agent_name: "researcher".to_string(),
                    status: AgentStatus::Completed,
                    session_id: Some("session-record".to_string()),
                    final_answer: Some("record done".to_string()),
                    wait_reason: None,
                    error: None,
                    cycles: 3,
                    todo_list: Vec::new(),
                    resolved: BTreeMap::from([("backend".to_string(), "deepseek".to_string())]),
                }
            },
        )
        .expect("submit record sub-task");

    let running = manager.get("sub-record").expect("running record snapshot");
    assert_eq!(running.task_id, "sub-record");
    assert_eq!(running.session_id, "session-record");
    assert_eq!(running.agent_name, "researcher");
    assert_eq!(running.task_title, "first run");

    let completed = manager
        .wait_for_record("sub-record", Some(Duration::from_secs(5)))
        .expect("completed record snapshot");
    assert_eq!(completed.status, "completed");
    assert_eq!(completed.current_cycle_index, Some(3));
    assert_eq!(completed.recent_activity.as_deref(), Some("record done"));
    assert_eq!(
        completed
            .outcome
            .as_ref()
            .and_then(|outcome| outcome.final_answer.as_deref()),
        Some("record done")
    );
    assert_eq!(
        completed.resolved.get("backend").map(String::as_str),
        Some("deepseek")
    );
    assert!(!completed.running);
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
fn sub_task_status_coerces_task_ids_message_and_workspace_limit() {
    let _registry_lock = isolated_sub_agent_registry();
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::write(workspace.path().join("a.txt"), "a").expect("a");
    std::fs::write(workspace.path().join("b.txt"), "b").expect("b");
    std::fs::write(workspace.path().join("c.txt"), "c").expect("c");
    let registry = build_default_registry();
    let manager = SubTaskManager::default();
    let mut context = ToolContext::new(workspace.path());
    context.sub_task_manager = Some(manager.clone());
    let received = Arc::new(Mutex::new(Vec::<String>::new()));
    let session = Arc::new(RecordingSubAgentSession {
        received: Arc::clone(&received),
    });
    register_sub_agent_session("session-42", session.clone());
    manager
        .submit("42", "session-42", "researcher", "Numeric task id", || {
            thread::sleep(Duration::from_millis(100));
            SubTaskOutcome {
                task_id: "42".to_string(),
                agent_name: "researcher".to_string(),
                status: AgentStatus::Completed,
                session_id: Some("session-42".to_string()),
                final_answer: Some("done".to_string()),
                wait_reason: None,
                error: None,
                cycles: 1,
                todo_list: Vec::new(),
                resolved: BTreeMap::new(),
            }
        })
        .expect("submit numeric task id");
    manager.attach_session(
        "42",
        "session-42",
        "researcher",
        "Numeric task id",
        context.workspace_backend.clone(),
        session,
    );

    let result = registry
        .execute(
            &ToolCall::new(
                "sub_status_coerce",
                "sub_task_status",
                BTreeMap::from([
                    ("task_ids".to_string(), json!([42, 42, ""])),
                    ("message".to_string(), json!(12345)),
                    ("detail_level".to_string(), json!("snapshot")),
                    ("workspace_file_limit".to_string(), json!("2")),
                ]),
            ),
            &mut context,
        )
        .expect("sub_task_status argument normalization");

    unregister_sub_agent_session("session-42");
    assert_eq!(result.status, ToolResultStatus::Success);
    assert_eq!(received.lock().expect("received").as_slice(), ["12345"]);
    let payload: Value = serde_json::from_str(&result.content).expect("payload");
    assert_eq!(payload["tasks"].as_array().expect("tasks").len(), 1);
    assert_eq!(payload["tasks"][0]["task_id"], "42");
    assert_eq!(
        payload["tasks"][0]["snapshot"]["workspace_files"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        payload["tasks"][0]["snapshot"]["workspace_files_truncated"],
        true
    );
}

#[test]
fn sub_task_status_uses_json_truthiness_for_ids_and_limit_defaults() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    context.sub_task_manager = Some(SubTaskManager::default());

    let result = registry
        .execute(
            &ToolCall::new(
                "sub_status_truthy",
                "sub_task_status",
                BTreeMap::from([
                    ("task_ids".to_string(), json!([0, false, "known", "known"])),
                    ("workspace_file_limit".to_string(), json!(false)),
                ]),
            ),
            &mut context,
        )
        .expect("sub_task_status truthiness");

    assert_eq!(result.status, ToolResultStatus::Success);
    let payload: Value = serde_json::from_str(&result.content).expect("payload");
    let task_ids = payload["tasks"]
        .as_array()
        .expect("tasks")
        .iter()
        .map(|task| task["task_id"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(task_ids, vec!["known"]);
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
    manager
        .submit(
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
        )
        .expect("submit running sub-task");

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
fn sub_agent_session_private_unregister_removes_only_matching_session() {
    let _registry_lock = isolated_sub_agent_registry();
    let first: Arc<dyn SubAgentSession> = Arc::new(RecordingSubAgentSession {
        received: Arc::new(Mutex::new(Vec::new())),
    });
    let second: Arc<dyn SubAgentSession> = Arc::new(RecordingSubAgentSession {
        received: Arc::new(Mutex::new(Vec::new())),
    });

    _register_sub_agent_session("guarded-session", first.clone());
    _unregister_sub_agent_session("guarded-session", Some(second.clone()));
    assert!(sub_agent_session_registry()
        .get("guarded-session")
        .is_some());

    _unregister_sub_agent_session("guarded-session", Some(first));
    assert!(sub_agent_session_registry()
        .get("guarded-session")
        .is_none());
}
