use super::*;

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
            error_code: None,
            completion_reason: None,
            completion_tool_name: None,
            partial_output: None,
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
        error_code: None,
        completion_reason: None,
        completion_tool_name: None,
        partial_output: None,
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
            error_code: None,
            completion_reason: None,
            completion_tool_name: None,
            partial_output: None,
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
fn create_sub_task_rejects_non_boolean_arguments() {
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
            error_code: None,
            completion_reason: None,
            completion_tool_name: None,
            partial_output: None,
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

    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(result.error_code.as_deref(), Some("invalid_tool_arguments"));
    let payload: Value = serde_json::from_str(&result.content).expect("payload");
    assert_eq!(payload["error_code"], "invalid_tool_arguments");
    assert!(captured_flags.lock().expect("captured flags").is_empty());
}

#[test]
fn create_sub_task_rejects_scalar_text_arguments() {
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
            task_id: "sub_scalar".to_string(),
            agent_name: request.agent_name,
            status: AgentStatus::Completed,
            session_id: None,
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
    }));

    let result = registry
        .execute(
            &ToolCall::new(
                "sub_scalar",
                "create_sub_task",
                BTreeMap::from([
                    ("agent_id".to_string(), json!(42)),
                    ("task_description".to_string(), json!(12345)),
                    ("output_requirements".to_string(), json!(false)),
                    ("exclude_files_pattern".to_string(), json!(99)),
                ]),
            ),
            &mut context,
        )
        .expect("create_sub_task scalar arguments");

    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(result.error_code.as_deref(), Some("invalid_tool_arguments"));
    let captured = captured.lock().expect("captured");
    assert!(captured.is_empty());
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
fn create_sub_task_rejects_non_array_batch_payload() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    context.sub_task_runner = Some(Arc::new(|request| SubTaskOutcome {
        task_id: "sub_never".to_string(),
        agent_name: request.agent_name,
        status: AgentStatus::Completed,
        session_id: None,
        final_answer: Some("should not run".to_string()),
        wait_reason: None,
        error: None,
        error_code: None,
        completion_reason: None,
        completion_tool_name: None,
        partial_output: None,
        cycles: 0,
        todo_list: Vec::new(),
        resolved: BTreeMap::new(),
    }));

    let result = registry
        .execute(
            &ToolCall::new(
                "sub_bad_tasks",
                "create_sub_task",
                BTreeMap::from([
                    ("agent_id".to_string(), json!("research-sub")),
                    ("tasks".to_string(), json!("not a list")),
                ]),
            ),
            &mut context,
        )
        .expect("create_sub_task");

    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(result.error_code.as_deref(), Some("invalid_tool_arguments"));
    let payload: Value = serde_json::from_str(&result.content).expect("payload");
    assert_eq!(payload["error_code"], "invalid_tool_arguments");
}

#[test]
fn create_sub_task_rejects_batch_with_any_schema_invalid_item() {
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
        error_code: None,
        completion_reason: None,
        completion_tool_name: None,
        partial_output: None,
        cycles: 1,
        todo_list: Vec::new(),
        resolved: BTreeMap::new(),
    }));

    let result = registry
        .execute(
            &ToolCall::new(
                "sub_mixed_invalid",
                "create_sub_task",
                BTreeMap::from([
                    ("agent_id".to_string(), json!("research-sub")),
                    (
                        "tasks".to_string(),
                        json!(["not an object", {"task_description": "Collect facts"}]),
                    ),
                ]),
            ),
            &mut context,
        )
        .expect("create_sub_task");

    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(result.error_code.as_deref(), Some("invalid_tool_arguments"));
    let payload: Value = serde_json::from_str(&result.content).expect("payload");
    assert_eq!(payload["error_code"], "invalid_tool_arguments");
}

#[test]
fn create_sub_task_batch_all_runtime_failures_uses_structured_error_envelope() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    context.sub_task_runner = Some(Arc::new(|request| SubTaskOutcome {
        task_id: format!("sub_{}", request.task_description.replace(' ', "_")),
        agent_name: request.agent_name,
        status: AgentStatus::Failed,
        session_id: None,
        final_answer: None,
        wait_reason: None,
        error: Some(format!("failed: {}", request.task_description)),
        error_code: None,
        completion_reason: None,
        completion_tool_name: None,
        partial_output: None,
        cycles: 1,
        todo_list: Vec::new(),
        resolved: BTreeMap::new(),
    }));

    let result = registry
        .execute(
            &ToolCall::new(
                "sub_batch_failed",
                "create_sub_task",
                BTreeMap::from([
                    ("agent_id".to_string(), json!("research-sub")),
                    (
                        "tasks".to_string(),
                        json!([
                            {"task_description": "Collect facts"},
                            {"task_description": "Write report"}
                        ]),
                    ),
                ]),
            ),
            &mut context,
        )
        .expect("create_sub_task");

    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(
        result.error_code.as_deref(),
        Some("create_sub_task_batch_failed")
    );
    let payload: Value = serde_json::from_str(&result.content).expect("payload");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"], "All batch sub-tasks failed");
    assert_eq!(payload["error_code"], "create_sub_task_batch_failed");
    assert_eq!(payload["details"]["summary"]["total"], 2);
    assert_eq!(payload["details"]["summary"]["completed"], 0);
    assert_eq!(payload["details"]["summary"]["failed"], 2);
    assert_eq!(payload["details"]["results"][0]["status"], "failed");
    assert_eq!(
        payload["details"]["results"][0]["error"],
        "failed: Collect facts"
    );
    assert_eq!(payload["details"]["wait_for_completion"], true);
}
