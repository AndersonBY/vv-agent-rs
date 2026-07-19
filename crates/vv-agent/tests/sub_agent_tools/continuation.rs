use super::*;

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
            error_code: None,
            completion_reason: None,
            completion_tool_name: None,
            partial_output: None,
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
            error_code: None,
            completion_reason: None,
            completion_tool_name: None,
            partial_output: None,
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
fn sub_task_manager_preserves_attached_resolved_payload_for_continuation() {
    let _registry_lock = isolated_sub_agent_registry();
    let workspace = tempfile::tempdir().expect("workspace");
    let manager = SubTaskManager::default();
    let continued = Arc::new(Mutex::new(Vec::<String>::new()));
    let session = Arc::new(ContinuingSubAgentSession {
        continued: Arc::clone(&continued),
    });
    manager.attach_session_with_resolved(SubTaskSessionAttachment {
        task_id: "sub-task-completed".to_string(),
        session_id: "sub-session-continued".to_string(),
        agent_name: "researcher".to_string(),
        task_title: "initial task".to_string(),
        workspace_backend: Arc::new(vv_agent::workspace::LocalWorkspaceBackend::new(
            workspace.path(),
        )),
        session,
        resolved: BTreeMap::from([
            ("backend".to_string(), "moonshot".to_string()),
            ("model_id".to_string(), "kimi-k2.5".to_string()),
        ]),
    });
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
            error_code: None,
            completion_reason: None,
            completion_tool_name: None,
            partial_output: None,
            cycles: 1,
            todo_list: Vec::new(),
            resolved: BTreeMap::new(),
        },
    );

    manager
        .continue_task("sub-task-completed", "add appendix")
        .expect("continue sub task");
    assert!(manager.wait("sub-task-completed", Some(Duration::from_secs(5))));

    let entries = manager.status_entries(&["sub-task-completed".to_string()], "basic", 10);
    assert_eq!(entries[0]["status"], "completed");
    assert_eq!(entries[0]["resolved"]["backend"], "moonshot");
    assert_eq!(entries[0]["resolved"]["model_id"], "kimi-k2.5");
}

#[test]
fn sub_task_manager_records_failed_outcome_when_background_runner_panics() {
    let manager = SubTaskManager::default();

    manager
        .submit(
            "sub-task-panic",
            "sub-session-panic",
            "researcher",
            "panic task",
            || -> SubTaskOutcome { panic!("runner exploded") },
        )
        .expect("submit panic task");

    assert!(manager.wait("sub-task-panic", Some(Duration::from_secs(5))));
    let entries = manager.status_entries(&["sub-task-panic".to_string()], "basic", 10);

    assert_eq!(entries[0]["status"], "failed");
    assert!(entries[0]["error"]
        .as_str()
        .expect("panic error")
        .contains("runner exploded"));
    assert_eq!(entries[0]["error_code"], "sub_task_failed");
}

#[test]
fn sub_task_manager_sanitizes_session_messages_before_continue() {
    let _registry_lock = isolated_sub_agent_registry();
    let manager = SubTaskManager::default();
    let workspace = tempfile::tempdir().expect("workspace");
    let reasoning_contract: Value = serde_json::from_str(include_str!(
        "../fixtures/parity/assistant_reasoning_history_v1.json"
    ))
    .expect("assistant reasoning history fixture");
    let cases = reasoning_contract["cases"]
        .as_array()
        .expect("assistant reasoning cases");
    let reasoning_case = cases
        .iter()
        .find(|case| case["name"] == "reasoning_only_assistant_is_preserved")
        .expect("reasoning-only assistant case");
    let empty_case = cases
        .iter()
        .find(|case| case["name"] == "fully_empty_assistant_is_removed")
        .expect("fully empty assistant case");
    let mut reasoning_message = Message::assistant(
        reasoning_case["message"]["content"]
            .as_str()
            .expect("reasoning visible content"),
    );
    reasoning_message.reasoning_content = Some(
        reasoning_case["message"]["reasoning_content"]
            .as_str()
            .expect("reasoning content")
            .to_string(),
    );
    let messages = Arc::new(Mutex::new(vec![
        Message::system("sys"),
        reasoning_message.clone(),
        Message::assistant(
            empty_case["message"]["content"]
                .as_str()
                .expect("empty assistant content"),
        ),
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
            error_code: None,
            completion_reason: None,
            completion_tool_name: None,
            partial_output: None,
            cycles: 1,
            todo_list: Vec::new(),
            resolved: BTreeMap::new(),
        },
    );

    manager
        .continue_task("sub-sanitize", "resume")
        .expect("continue sub task");
    assert!(manager.wait("sub-sanitize", Some(Duration::from_secs(5))));

    assert!(reasoning_case["expected"]["retain_in_resumable_history"] == true);
    assert!(empty_case["expected"]["retain_in_resumable_history"] == false);
    assert_eq!(
        snapshot.lock().expect("snapshot").as_slice(),
        &[Message::system("sys"), reasoning_message]
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
            error_code: None,
            completion_reason: None,
            completion_tool_name: None,
            partial_output: None,
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
    manager
        .submit(
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
                    error_code: None,
                    completion_reason: None,
                    completion_tool_name: None,
                    partial_output: None,
                    cycles: 1,
                    todo_list: Vec::new(),
                    resolved: BTreeMap::new(),
                }
            },
        )
        .expect("submit snapshot sub-task");
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
    let updated_at = task["snapshot"]["updated_at"]
        .as_str()
        .expect("snapshot updated_at should be an ISO timestamp string");
    assert!(
        updated_at.contains('T') && updated_at.ends_with("+00:00"),
        "snapshot updated_at should use UTC ISO timestamp format, got {updated_at:?}"
    );
    assert_eq!(task["snapshot"]["workspace_files"], json!(["notes.md"]));
    assert_eq!(task["snapshot"]["workspace_file_count"], 1);
    assert_eq!(task["snapshot"]["workspace_files_truncated"], false);
}
