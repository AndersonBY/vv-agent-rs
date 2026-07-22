use super::*;

#[test]
fn manager_outcome_identity_blank_code_and_unicode_preview_match_contract() {
    let fixture = manager_tool_contract();
    let contract = &fixture["manager_outcome"];
    let lookup_task_id = contract["lookup_task_id"].as_str().expect("lookup task id");
    let manager = SubTaskManager::default();
    manager.record_outcome(
        lookup_task_id,
        SubTaskOutcome {
            task_id: contract["outcome_task_id"]
                .as_str()
                .expect("outcome task id")
                .to_string(),
            agent_name: "researcher".to_string(),
            status: AgentStatus::Failed,
            session_id: Some("wire-session".to_string()),
            final_answer: None,
            wait_reason: None,
            error: Some("child failed".to_string()),
            error_code: Some(" ".to_string()),
            completion_reason: None,
            completion_tool_name: None,
            partial_output: None,
            cycles: 0,
            todo_list: Vec::new(),
            resolved: BTreeMap::new(),
        },
    );

    let snapshot = manager.get(lookup_task_id).expect("lookup snapshot");
    assert_eq!(snapshot.task_id, lookup_task_id);
    let status = manager.status_entries(&[lookup_task_id.to_string()], "basic", 20);
    assert_eq!(status[0], contract["status_entry"]);

    let preview_contract = &contract["unicode_preview"];
    let text = preview_contract["text"]
        .as_str()
        .expect("preview text")
        .repeat(preview_contract["repeat"].as_u64().expect("preview repeat") as usize);
    manager.record_outcome(
        "unicode-preview",
        SubTaskOutcome {
            task_id: "unicode-preview-wire".to_string(),
            agent_name: "researcher".to_string(),
            status: AgentStatus::Completed,
            session_id: Some("unicode-preview-session".to_string()),
            final_answer: Some(text.clone()),
            wait_reason: None,
            error: None,
            error_code: None,
            completion_reason: None,
            completion_tool_name: None,
            partial_output: None,
            cycles: 0,
            todo_list: Vec::new(),
            resolved: BTreeMap::new(),
        },
    );
    assert_eq!(
        manager
            .get("unicode-preview")
            .expect("unicode preview snapshot")
            .recent_activity
            .as_deref(),
        Some(text.as_str())
    );
}

struct PromptCaptureSession {
    prompts: Arc<Mutex<Vec<String>>>,
    task_id: String,
    session_id: String,
}

impl SubAgentSession for PromptCaptureSession {
    fn steer(&self, _prompt: &str) -> Result<(), String> {
        Ok(())
    }

    fn continue_run(&self, prompt: &str) -> Result<SubTaskOutcome, String> {
        self.prompts
            .lock()
            .expect("captured continuation prompts")
            .push(prompt.to_string());
        Ok(generation_outcome(
            &self.task_id,
            &self.session_id,
            "continued",
        ))
    }
}

fn generation_outcome(task_id: &str, session_id: &str, answer: &str) -> SubTaskOutcome {
    SubTaskOutcome {
        task_id: task_id.to_string(),
        agent_name: "researcher".to_string(),
        status: AgentStatus::Completed,
        session_id: Some(session_id.to_string()),
        final_answer: Some(answer.to_string()),
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
}

#[test]
fn direct_manager_task_ids_are_exact_while_prompt_and_tool_ingress_are_trimmed() {
    let fixture = contract();
    let manager_contract = &fixture["manager"];
    let manager = SubTaskManager::default();
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let exact_task_id = " task-with-spaces ";
    manager.attach_session(
        exact_task_id,
        "exact-session",
        "researcher",
        "initial",
        Arc::new(MemoryWorkspaceBackend::default()),
        Arc::new(PromptCaptureSession {
            prompts: prompts.clone(),
            task_id: exact_task_id.to_string(),
            session_id: "exact-session".to_string(),
        }),
    );

    let error = manager
        .continue_task("task-with-spaces", "continue")
        .expect_err("direct manager lookup must not trim task ids");
    assert!(error.contains("Sub-task task-with-spaces not found."));
    manager
        .continue_task(exact_task_id, "\u{001c}continue now\u{001f}")
        .expect("exact manager key");
    assert!(manager.wait(exact_task_id, Some(Duration::from_secs(2))));
    assert_eq!(
        prompts.lock().expect("captured prompt").as_slice(),
        ["continue now"]
    );
    assert_eq!(manager_contract["task_id_policy"], "opaque_exact_key");

    manager.record_outcome(
        "tool-task",
        generation_outcome("tool-task", "tool-session", "done"),
    );
    let registry = build_default_registry();
    let mut context = ToolContext::new(".");
    context.sub_task_manager = Some(manager);
    let result = registry
        .execute(
            &ToolCall::new(
                "portable-task-id",
                "sub_task_status",
                BTreeMap::from([("task_ids".to_string(), json!(["\u{001c}tool-task\u{001f}"]))]),
            ),
            &mut context,
        )
        .expect("portable task-id ingress");
    assert_eq!(result.status, ToolResultStatus::Success);
    assert_eq!(
        manager_contract["tool_and_wire_ingress_trim_task_ids"],
        true
    );
}

#[test]
fn terminal_task_id_reuse_starts_a_new_generation() {
    let fixture = contract();
    let manager = SubTaskManager::default();
    manager
        .submit("reused-task", "session-one", "researcher", "first", || {
            generation_outcome("reused-task", "session-one", "first")
        })
        .expect("first generation");
    assert!(manager.wait("reused-task", Some(Duration::from_secs(2))));
    assert_eq!(
        manager
            .get("reused-task")
            .expect("first generation snapshot")
            .session_id,
        "session-one"
    );

    manager
        .submit("reused-task", "session-two", "researcher", "second", || {
            generation_outcome("reused-task", "session-two", "second")
        })
        .expect("terminal key starts a new generation");
    assert!(manager.wait("reused-task", Some(Duration::from_secs(2))));
    let snapshot = manager
        .get("reused-task")
        .expect("second generation snapshot");
    assert_eq!(snapshot.session_id, "session-two");
    assert_eq!(
        snapshot
            .outcome
            .as_ref()
            .and_then(|outcome| outcome.final_answer.as_deref()),
        Some("second")
    );
    assert_eq!(
        fixture["manager"]["terminal_task_id_reuse"],
        "new_generation"
    );
}

#[test]
fn async_submission_behavior_matches_fixture() {
    let fixture = contract();
    let async_contract = &fixture["manager"]["async_submission"];
    let registry = build_default_registry();

    let mut single_context = ToolContext::new(".");
    single_context.task_id = "async-single-parent".to_string();
    single_context.sub_task_manager = Some(SubTaskManager::default());
    single_context.sub_task_runner = Some(Arc::new(completed_outcome));
    let single = registry
        .execute(
            &ToolCall::new(
                "single-submit-failure",
                "create_sub_task",
                BTreeMap::from([
                    ("agent_id".to_string(), json!("invalid\0thread")),
                    ("task_description".to_string(), json!("fail to spawn")),
                    ("wait_for_completion".to_string(), json!(false)),
                ]),
            ),
            &mut single_context,
        )
        .expect("single async submit failure");
    assert_eq!(
        single.error_code.as_deref(),
        async_contract["single_error_code"].as_str()
    );

    assert_eq!(
        async_contract["batch_continues_after_submission_failure"],
        true
    );

    let mut failed_context = ToolContext::new(".");
    failed_context.task_id = "async-all-failed-parent".to_string();
    failed_context.sub_task_manager = Some(SubTaskManager::default());
    failed_context.sub_task_runner = Some(Arc::new(completed_outcome));
    let all_failed = registry
        .execute(
            &ToolCall::new(
                "all-submit-failures",
                "create_sub_task",
                BTreeMap::from([
                    ("agent_id".to_string(), json!("invalid\0thread")),
                    (
                        "tasks".to_string(),
                        json!([{"task_description": "first"}, {"task_description": "second"}]),
                    ),
                    ("wait_for_completion".to_string(), json!(false)),
                ]),
            ),
            &mut failed_context,
        )
        .expect("all failed async batch");
    assert_eq!(
        all_failed.error_code.as_deref(),
        async_contract["all_failed_error_code"].as_str()
    );
}

#[test]
fn same_model_parent_client_inherits_fixture_token_limits() {
    let fixture = contract();
    let model_contract = &fixture["model_resolution"];
    let limits = &model_contract["resolved_token_limits"];
    let child_request = Arc::new(Mutex::new(None::<LlmRequest>));
    let child_request_for_step = child_request.clone();
    let llm = ScriptedLlmClient::from_steps(vec![
        ScriptStep::response(LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::from_raw_arguments(
                "delegate",
                "create_sub_task",
                json!({"agent_id": "researcher", "task_description": "inherit limits"}),
            )],
        )),
        ScriptStep::callback(move |request| {
            *child_request_for_step
                .lock()
                .expect("captured child request") = Some(request.clone());
            Ok(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "child-finish",
                    "task_finish",
                    json!({"message": "child done"}),
                )],
            ))
        }),
        ScriptStep::response(LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::from_raw_arguments(
                "parent-finish",
                "task_finish",
                json!({"message": "parent done"}),
            )],
        )),
    ]);
    let mut parent = AgentTask::new("limit-parent", "shared-model", "Parent", "Delegate");
    parent.max_cycles = 2;
    parent.metadata.insert(
        "model_context_window".to_string(),
        limits["context_length"].clone(),
    );
    parent.metadata.insert(
        "model_max_output_tokens".to_string(),
        limits["max_output_tokens"].clone(),
    );
    let mut child = SubAgentConfig::new("shared-model", "Research");
    child.system_prompt = Some("Child prompt".to_string());
    parent.sub_agents.insert("researcher".to_string(), child);

    let result = AgentRuntime::new(llm)
        .run(parent)
        .expect("same-model configured child run");
    assert_eq!(result.status, AgentStatus::Completed);
    let request = child_request
        .lock()
        .expect("captured child request")
        .clone()
        .expect("child request");
    assert_eq!(
        request.metadata["model_context_window"],
        limits["context_length"]
    );
    assert_eq!(
        request.metadata["model_max_output_tokens"],
        limits["max_output_tokens"]
    );
    assert!(request
        .model_settings
        .as_ref()
        .and_then(|settings| settings.max_tokens)
        .is_none());
    assert!(request.metadata.get("reserved_output_tokens").is_none());
    assert_eq!(
        model_contract["same_model_parent_client_inherits_token_limits"],
        true
    );
}
