use super::manager_support::*;
use super::*;
use std::panic::{catch_unwind, AssertUnwindSafe};

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

#[test]
fn listener_subscription_failure_retries_and_old_session_events_are_ignored() {
    let fixture = manager_tool_contract();
    let contract = &fixture["listener_identity"];
    let manager = SubTaskManager::default();
    let failing = Arc::new(FailingSubscribeSession {
        listeners: Mutex::new(Vec::new()),
        subscribe_calls: AtomicUsize::new(0),
    });
    let first = catch_unwind(AssertUnwindSafe(|| {
        manager.attach_session(
            "listener-task",
            "listener-session-a",
            "researcher",
            "initial",
            Arc::new(MemoryWorkspaceBackend::default()),
            failing.clone(),
        );
    }));
    assert!(first.is_err());
    manager.attach_session(
        "listener-task",
        "listener-session-a",
        "researcher",
        "retry",
        Arc::new(MemoryWorkspaceBackend::default()),
        failing.clone(),
    );
    assert!(failing.subscribe_calls.load(Ordering::SeqCst) >= 2);
    failing.emit(
        "cycle_llm_response",
        BTreeMap::from([("assistant_preview".to_string(), json!("current"))]),
    );

    let replacement = Arc::new(ListenerRetainingSession::default());
    manager.attach_session(
        "listener-task",
        "listener-session-b",
        "researcher",
        "replacement",
        Arc::new(MemoryWorkspaceBackend::default()),
        replacement.clone(),
    );
    let before_stale = manager.get("listener-task").expect("replacement snapshot");
    failing.emit(
        "cycle_llm_response",
        BTreeMap::from([("assistant_preview".to_string(), json!("stale"))]),
    );
    let after_stale = manager.get("listener-task").expect("stale snapshot");

    assert_eq!(after_stale.session_id, "listener-session-b");
    assert_eq!(after_stale.recent_activity, before_stale.recent_activity);
    assert_eq!(contract["subscribe_failure_retry"], true);
    assert_eq!(contract["stale_session_events_ignored"], true);
}

#[test]
fn running_worker_hides_early_recorded_outcome_until_exit() {
    let fixture = manager_tool_contract();
    let contract = &fixture["worker_visibility"];
    let manager = SubTaskManager::default();
    let (recorded_tx, recorded_rx) = std::sync::mpsc::channel();
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let release_for_runner = release.clone();
    let manager_for_runner = manager.clone();
    manager
        .submit(
            "worker-task",
            "worker-session",
            "researcher",
            "worker",
            move || {
                let outcome = SubTaskOutcome {
                    task_id: "worker-task".to_string(),
                    agent_name: "researcher".to_string(),
                    status: AgentStatus::Completed,
                    session_id: Some("worker-session".to_string()),
                    final_answer: Some("early terminal".to_string()),
                    wait_reason: None,
                    error: None,
                    error_code: None,
                    cycles: 0,
                    todo_list: Vec::new(),
                    resolved: BTreeMap::new(),
                };
                manager_for_runner.record_outcome("worker-task", outcome.clone());
                recorded_tx.send(()).expect("recorded early outcome");
                let (released, wake) = &*release_for_runner;
                let mut released = released.lock().expect("release lock");
                while !*released {
                    released = wake.wait(released).expect("release wait");
                }
                outcome
            },
        )
        .expect("submit worker");
    recorded_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("early outcome recorded");

    let snapshot = manager.get("worker-task").expect("running snapshot");
    assert!(snapshot.running);
    assert_eq!(snapshot.status, "running");
    assert!(snapshot.outcome.is_none());
    let status = manager.status_entries(&["worker-task".to_string()], "basic", 20);
    assert_eq!(status[0]["status"], "running");
    assert!(status[0].get("final_answer").is_none());
    assert!(!manager.wait("worker-task", Some(Duration::from_millis(20))));

    let (released, wake) = &*release;
    *released.lock().expect("release lock") = true;
    wake.notify_all();
    assert!(manager.wait("worker-task", Some(Duration::from_secs(2))));
    let completed = manager.get("worker-task").expect("completed snapshot");
    assert!(!completed.running);
    assert_eq!(
        completed
            .outcome
            .as_ref()
            .and_then(|outcome| outcome.final_answer.as_deref()),
        Some("early terminal")
    );
    assert_eq!(contract["running_status_authoritative"], true);
    assert_eq!(contract["terminal_fields_hidden_until_worker_exit"], true);
}

#[test]
fn submit_spawn_failure_does_not_leave_a_new_task_record() {
    let manager = SubTaskManager::default();
    let error = manager
        .submit_with_context(
            "new-spawn-failure",
            "spawn\0failure-session",
            "researcher",
            "must not remain",
            SubTaskSubmissionContext::default(),
            || failed_outcome("new-spawn-failure", "spawn-failure-session"),
        )
        .expect_err("invalid thread name must fail spawn");

    assert!(error.contains("failed to spawn"));
    assert!(manager.get("new-spawn-failure").is_none());
}

#[test]
fn submit_spawn_failure_restores_reused_task_snapshot_and_status_envelope() {
    let manager = SubTaskManager::default();
    let old_workspace = Arc::new(MemoryWorkspaceBackend::default());
    old_workspace
        .write_text("old.txt", "old", false)
        .expect("old workspace file");
    let old_session = Arc::new(ListenerRetainingSession::default());
    manager.attach_session_with_resolved_and_lineage(
        vv_agent::runtime::SubTaskSessionAttachment {
            task_id: "reused-task".to_string(),
            session_id: "old-session".to_string(),
            agent_name: "old-agent".to_string(),
            task_title: "old title".to_string(),
            workspace_backend: old_workspace,
            session: old_session.clone(),
            resolved: BTreeMap::from([("backend".to_string(), "old-backend".to_string())]),
        },
        SubTaskLineage {
            parent_run_id: Some("old-parent-run".to_string()),
            parent_tool_call_id: Some("old-parent-call".to_string()),
        },
    );
    old_session.emit(
        "cycle_llm_response",
        BTreeMap::from([
            ("cycle".to_string(), json!(2)),
            ("assistant_preview".to_string(), json!("old activity")),
        ]),
    );
    manager.record_outcome(
        "reused-task",
        SubTaskOutcome {
            task_id: "reused-task".to_string(),
            agent_name: "old-agent".to_string(),
            status: AgentStatus::Completed,
            session_id: Some("old-session".to_string()),
            final_answer: Some("old result".to_string()),
            wait_reason: None,
            error: None,
            error_code: None,
            cycles: 2,
            todo_list: vec![json!({"content": "old todo"})],
            resolved: BTreeMap::from([("model_id".to_string(), "old-model".to_string())]),
        },
    );
    let before = manager.get("reused-task").expect("old task snapshot");
    let before_status = manager.status_entries(&["reused-task".to_string()], "snapshot", 10);

    let replacement_workspace = Arc::new(MemoryWorkspaceBackend::default());
    replacement_workspace
        .write_text("replacement.txt", "replacement", false)
        .expect("replacement workspace file");
    let error = manager
        .submit_with_context(
            "reused-task",
            "spawn\0replacement-session",
            "replacement-agent",
            "replacement title",
            SubTaskSubmissionContext {
                workspace_backend: Some(replacement_workspace),
                lineage: SubTaskLineage {
                    parent_run_id: Some("replacement-parent-run".to_string()),
                    parent_tool_call_id: Some("replacement-parent-call".to_string()),
                },
            },
            || failed_outcome("reused-task", "replacement-session"),
        )
        .expect_err("invalid replacement thread name must fail spawn");

    assert!(error.contains("failed to spawn"));
    let after = manager
        .get("reused-task")
        .expect("reused task remains queryable");
    assert_same_managed_snapshot(&after, &before);
    assert_eq!(
        manager.status_entries(&["reused-task".to_string()], "snapshot", 10),
        before_status
    );
}

#[test]
fn manager_normalizes_failed_outcomes_from_every_ingestion_path() {
    let expected = manager_tool_contract()["failed_outcome_error_code"]
        .as_str()
        .expect("failed outcome fallback")
        .to_string();
    let manager = SubTaskManager::default();

    manager
        .submit(
            "submitted-task",
            "submitted-session",
            "researcher",
            "submitted failure",
            || failed_outcome("submitted-task", "submitted-session"),
        )
        .expect("submit failed outcome");
    assert!(manager.wait("submitted-task", Some(Duration::from_secs(2))));
    assert_failed_outcome_code(&manager, "submitted-task", &expected);

    manager.record_outcome(
        "recorded-task",
        failed_outcome("recorded-task", "recorded-session"),
    );
    assert_failed_outcome_code(&manager, "recorded-task", &expected);

    manager.attach_session(
        "continued-task",
        "continued-session",
        "researcher",
        "continued failure",
        Arc::new(MemoryWorkspaceBackend::default()),
        Arc::new(FailedContinuationSession),
    );
    manager
        .continue_task("continued-task", "continue")
        .expect("continue failed outcome");
    assert!(manager.wait("continued-task", Some(Duration::from_secs(2))));
    assert_failed_outcome_code(&manager, "continued-task", &expected);
}

#[test]
fn manager_listener_updates_live_state_without_retaining_manager_session_cycle() {
    let manager = SubTaskManager::default();
    let session = Arc::new(ListenerRetainingSession::default());
    let weak_session = Arc::downgrade(&session);
    manager.attach_session_with_resolved_and_lineage(
        vv_agent::runtime::SubTaskSessionAttachment {
            task_id: "listener-task".to_string(),
            session_id: "listener-session".to_string(),
            agent_name: "researcher".to_string(),
            task_title: "Listen for progress".to_string(),
            workspace_backend: Arc::new(MemoryWorkspaceBackend::default()),
            session: session.clone(),
            resolved: BTreeMap::new(),
        },
        SubTaskLineage {
            parent_run_id: Some("parent-run".to_string()),
            parent_tool_call_id: Some("delegate".to_string()),
        },
    );

    session.emit(
        "cycle_started",
        BTreeMap::from([("cycle".to_string(), json!(3))]),
    );
    assert_eq!(
        manager
            .get("listener-task")
            .expect("listener task snapshot")
            .current_cycle_index,
        Some(3)
    );

    drop(session);
    assert!(weak_session.upgrade().is_some());
    drop(manager);
    assert!(
        weak_session.upgrade().is_none(),
        "manager and retained session listener must not form an Arc cycle"
    );
}

#[test]
fn status_snapshot_fixture_preserves_lineage_and_omits_unknown_recent_activity() {
    let fixture = manager_tool_contract();
    let manager = SubTaskManager::default();
    manager.attach_session_with_resolved_and_lineage(
        vv_agent::runtime::SubTaskSessionAttachment {
            task_id: "status-task".to_string(),
            session_id: "status-session".to_string(),
            agent_name: "researcher".to_string(),
            task_title: "Inspect status".to_string(),
            workspace_backend: Arc::new(MemoryWorkspaceBackend::default()),
            session: Arc::new(ListenerRetainingSession::default()),
            resolved: BTreeMap::new(),
        },
        SubTaskLineage {
            parent_run_id: Some("parent-run".to_string()),
            parent_tool_call_id: Some("delegate".to_string()),
        },
    );

    let mut context = ToolContext::new(".");
    context.sub_task_manager = Some(manager);
    let result = build_default_registry()
        .execute(
            &ToolCall::new(
                "status-envelope",
                "sub_task_status",
                BTreeMap::from([
                    ("task_ids".to_string(), json!(["status-task"])),
                    ("detail_level".to_string(), json!("snapshot")),
                ]),
            ),
            &mut context,
        )
        .expect("sub_task_status envelope");
    assert_eq!(result.status, ToolResultStatus::Success);
    let payload: Value = serde_json::from_str(&result.content).expect("status envelope payload");
    let entry = &payload["tasks"][0];
    let lineage_fields = fixture["status_envelope"]["lineage_fields"]
        .as_array()
        .expect("lineage fields");
    assert!(lineage_fields
        .iter()
        .filter_map(Value::as_str)
        .all(|field| entry.get(field).is_some()));
    assert_eq!(entry["parent_run_id"], "parent-run");
    assert_eq!(entry["parent_tool_call_id"], "delegate");
    assert_eq!(
        entry["snapshot"].get("recent_activity").is_none(),
        fixture["status_envelope"]["recent_activity_when_unavailable"] == "omitted"
    );
}

#[test]
fn async_manager_snapshot_uses_discovery_filtered_backend_before_child_finishes() {
    let registry = build_default_registry();
    let backend = Arc::new(MemoryWorkspaceBackend::default());
    backend
        .write_text("src/main.py", "visible", false)
        .expect("visible file");
    backend
        .write_text("generated/cache.bin", "hidden", false)
        .expect("hidden file");
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let gate_for_runner = gate.clone();
    let manager = SubTaskManager::default();
    let mut context = ToolContext::new(".");
    context.task_id = "parent-task".to_string();
    context.workspace_backend = backend;
    context.sub_task_manager = Some(manager.clone());
    context.sub_task_runner = Some(Arc::new(move |request| {
        let (released, wake) = &*gate_for_runner;
        let mut released = released.lock().expect("gate lock");
        while !*released {
            released = wake.wait(released).expect("gate wait");
        }
        completed_outcome(request)
    }));

    let result = registry
        .execute(
            &ToolCall::new(
                "async-filter",
                "create_sub_task",
                BTreeMap::from([
                    ("agent_id".to_string(), json!("researcher")),
                    ("task_description".to_string(), json!("inspect workspace")),
                    ("exclude_files_pattern".to_string(), json!("^generated/")),
                    ("wait_for_completion".to_string(), json!(false)),
                ]),
            ),
            &mut context,
        )
        .expect("start filtered async task");
    let payload: Value = serde_json::from_str(&result.content).expect("async payload");
    let task_id = payload["task_id"].as_str().expect("task id");
    let status = manager.status_entries(&[task_id.to_string()], "snapshot", 10);
    assert_eq!(
        status[0]["snapshot"]["workspace_files"],
        json!(["src/main.py"])
    );

    let (released, wake) = &*gate;
    *released.lock().expect("gate lock") = true;
    wake.notify_all();
    assert!(manager.wait(task_id, Some(Duration::from_secs(2))));
}

#[test]
fn manager_timeout_keeps_queryable_lineage_without_fabricating_terminal_state() {
    let fixture = contract();
    let manager_contract = &fixture["manager"];
    let manager = SubTaskManager::default();
    manager
        .submit_with_context(
            "child-task",
            "child-session",
            "researcher",
            "slow task",
            SubTaskSubmissionContext {
                workspace_backend: None,
                lineage: SubTaskLineage {
                    parent_run_id: Some("parent-run".to_string()),
                    parent_tool_call_id: Some("delegate".to_string()),
                },
            },
            || {
                thread::sleep(Duration::from_millis(80));
                SubTaskOutcome {
                    task_id: "child-task".to_string(),
                    agent_name: "researcher".to_string(),
                    status: AgentStatus::Completed,
                    session_id: Some("child-session".to_string()),
                    final_answer: Some("done".to_string()),
                    wait_reason: None,
                    error: None,
                    error_code: None,
                    cycles: 1,
                    todo_list: Vec::new(),
                    resolved: BTreeMap::new(),
                }
            },
        )
        .expect("submit task");

    assert!(!manager.wait("child-task", Some(Duration::from_millis(1))));
    let timed_out = manager.get("child-task").expect("snapshot after timeout");
    assert_eq!(
        timed_out.outcome.is_some(),
        manager_contract["timeout_fabricates_terminal_state"]
    );
    assert_eq!(timed_out.parent_run_id.as_deref(), Some("parent-run"));
    assert_eq!(timed_out.parent_tool_call_id.as_deref(), Some("delegate"));
    assert_eq!(
        manager.get("child-task").is_some(),
        manager_contract["snapshot_remains_queryable_after_timeout"]
    );
    let running_rejection = manager
        .continue_task("child-task", "continue too early")
        .expect_err("running task cannot be continued");
    assert!(running_rejection.contains("already running"));
    assert_eq!(
        !running_rejection.is_empty(),
        manager_contract["continue_running_rejected"]
    );
    let status = manager.status_entries(&["child-task".to_string()], "snapshot", 10);
    assert_eq!(status[0]["status"], "running");
    assert!(status[0].get("snapshot").is_some());
    assert!(manager.wait("child-task", Some(Duration::from_secs(2))));
}

#[test]
fn manager_rejects_max_cycle_continuation_and_reports_unattached_session_code() {
    let fixture = contract();
    let manager_contract = &fixture["manager"];
    let manager = SubTaskManager::default();
    manager.record_outcome(
        "maxed-task",
        SubTaskOutcome {
            task_id: "maxed-task".to_string(),
            agent_name: "researcher".to_string(),
            status: AgentStatus::MaxCycles,
            session_id: Some("maxed-session".to_string()),
            final_answer: None,
            wait_reason: None,
            error: None,
            error_code: None,
            cycles: 1,
            todo_list: Vec::new(),
            resolved: BTreeMap::new(),
        },
    );
    let max_cycles_error = manager
        .continue_task("maxed-task", "continue")
        .expect_err("max-cycle task cannot continue");
    assert!(max_cycles_error.contains("reached max cycles"));
    assert_eq!(
        !max_cycles_error.is_empty(),
        manager_contract["continue_max_cycles_rejected"]
    );

    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let release_for_runner = release.clone();
    manager
        .submit(
            "unattached-task",
            "unattached-session",
            "researcher",
            "Waiting for session attachment",
            move || {
                started_tx.send(()).expect("signal unattached task start");
                let (released, wake) = &*release_for_runner;
                let mut released = released.lock().expect("unattached release lock");
                while !*released {
                    released = wake.wait(released).expect("unattached release wait");
                }
                SubTaskOutcome {
                    task_id: "unattached-task".to_string(),
                    agent_name: "researcher".to_string(),
                    status: AgentStatus::Completed,
                    session_id: Some("unattached-session".to_string()),
                    final_answer: Some("done".to_string()),
                    wait_reason: None,
                    error: None,
                    error_code: None,
                    cycles: 1,
                    todo_list: Vec::new(),
                    resolved: BTreeMap::new(),
                }
            },
        )
        .expect("submit unattached task");
    started_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("unattached task started");
    let mut context = ToolContext::new(".");
    context.sub_task_manager = Some(manager.clone());
    let result = build_default_registry()
        .execute(
            &ToolCall::new(
                "status-message",
                "sub_task_status",
                BTreeMap::from([
                    ("task_ids".to_string(), json!(["unattached-task"])),
                    ("message".to_string(), json!("Focus on lineage")),
                ]),
            ),
            &mut context,
        )
        .expect("unattached status result");
    let (released, wake) = &*release;
    *released.lock().expect("unattached release lock") = true;
    wake.notify_all();
    assert!(manager.wait("unattached-task", Some(Duration::from_secs(2))));

    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(
        result.error_code.as_deref(),
        manager_contract["session_not_ready_error_code"].as_str()
    );
}

#[derive(Clone)]
struct FailingChildModelProvider {
    ordering: Arc<Mutex<Vec<String>>>,
}

impl ModelProvider for FailingChildModelProvider {
    fn resolve(&self, model: &ModelRef) -> Result<ResolvedModelConfig, ModelError> {
        assert_eq!(model.model(), "child-model");
        self.ordering
            .lock()
            .expect("resolution ordering")
            .push("model_resolution".to_string());
        Err(ModelError::Config("child model unavailable".to_string()))
    }

    fn client(&self, _resolved: &ResolvedModelConfig) -> Result<Arc<dyn LlmClient>, ModelError> {
        Err(ModelError::Config(
            "failing provider has no client".to_string(),
        ))
    }
}

#[derive(Clone)]
struct ExplicitBackendModelProvider {
    client: Arc<dyn LlmClient>,
    resolved_refs: Arc<Mutex<Vec<ModelRef>>>,
    context_length: u64,
    max_output_tokens: u64,
}

impl ModelProvider for ExplicitBackendModelProvider {
    fn resolve(&self, model: &ModelRef) -> Result<ResolvedModelConfig, ModelError> {
        self.resolved_refs
            .lock()
            .expect("explicit backend model refs")
            .push(model.clone());
        let backend = model
            .backend_name()
            .ok_or_else(|| ModelError::Config("explicit child backend was lost".to_string()))?;
        Ok(ResolvedModelConfig::new(
            backend,
            model.model(),
            model.model(),
            model.model(),
            Vec::new(),
        )
        .with_token_limits(Some(self.context_length), Some(self.max_output_tokens))
        .with_capabilities(true, true, false))
    }

    fn client(&self, _resolved: &ResolvedModelConfig) -> Result<Arc<dyn LlmClient>, ModelError> {
        Ok(self.client.clone())
    }
}

#[test]
fn explicit_backend_and_resolved_limits_reach_real_child_request_and_run_context() {
    let fixture = contract();
    let model_contract = &fixture["model_resolution"];
    let resolved_limits = &model_contract["resolved_token_limits"];
    let context_length = resolved_limits["context_length"]
        .as_u64()
        .expect("resolved context length");
    let max_output_tokens = resolved_limits["max_output_tokens"]
        .as_u64()
        .expect("resolved max output tokens");

    for (label, child_metadata, expected_context, expected_output) in [
        (
            "resolved limits",
            BTreeMap::new(),
            context_length,
            max_output_tokens,
        ),
        (
            "explicit child metadata wins",
            BTreeMap::from([
                ("model_context_window".to_string(), json!(12_345)),
                ("reserved_output_tokens".to_string(), json!(678)),
            ]),
            12_345,
            678,
        ),
    ] {
        let child_requests = Arc::new(Mutex::new(Vec::<LlmRequest>::new()));
        let child_requests_for_callback = child_requests.clone();
        let child_llm: Arc<dyn LlmClient> = Arc::new(ScriptedLlmClient::from_steps(vec![
            ScriptStep::callback(move |request| {
                child_requests_for_callback
                    .lock()
                    .expect("explicit backend child requests")
                    .push(request.clone());
                Ok(LLMResponse::with_tool_calls(
                    "",
                    vec![ToolCall::from_raw_arguments(
                        "inspect-model",
                        "inspect_child_model_ref",
                        json!({}),
                    )],
                ))
            }),
            ScriptStep::response(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "child-finish",
                    "task_finish",
                    json!({"message": "child done"}),
                )],
            )),
        ]));
        let resolved_refs = Arc::new(Mutex::new(Vec::new()));
        let provider: Arc<dyn ModelProvider> = Arc::new(ExplicitBackendModelProvider {
            client: child_llm,
            resolved_refs: resolved_refs.clone(),
            context_length,
            max_output_tokens,
        });
        let inspected_model = Arc::new(Mutex::new(None::<ModelRef>));
        let inspected_model_for_tool = inspected_model.clone();
        let mut registry = build_default_registry();
        registry
            .register(ToolSpec::new(
                "inspect_child_model_ref",
                "Inspect the configured child model reference.",
                Arc::new(move |context, _arguments| {
                    *inspected_model_for_tool
                        .lock()
                        .expect("inspected child model ref") = context
                        .run_context
                        .as_ref()
                        .and_then(|run| run.model.clone());
                    ToolExecutionResult::success("", json!({"ok": true}).to_string())
                }),
            ))
            .expect("register child model inspection tool");
        let parent_llm = ScriptedLlmClient::from_steps(vec![
            ScriptStep::response(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "delegate",
                    "create_sub_task",
                    json!({
                        "agent_id": "researcher",
                        "task_description": "Resolve child model"
                    }),
                )],
            )),
            ScriptStep::response(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "parent-finish",
                    "task_finish",
                    json!({"message": "parent done"}),
                )],
            )),
        ]);
        let manager = SubTaskManager::default();
        let mut parent = AgentTask::new(
            format!("explicit-backend-parent-{label}"),
            "parent-model",
            "Parent prompt",
            "Delegate",
        );
        parent.max_cycles = 3;
        parent.extra_tool_names = vec!["inspect_child_model_ref".to_string()];
        let mut child = SubAgentConfig::new("child-model", "Research");
        child.backend = Some("child-backend".to_string());
        child.system_prompt = Some("Child prompt".to_string());
        child.metadata = child_metadata;
        parent.sub_agents.insert("researcher".to_string(), child);

        let result = AgentRuntime::new(parent_llm)
            .with_tool_registry(registry)
            .run_with_controls(
                parent,
                RuntimeRunControls {
                    model_provider: Some(provider),
                    run_context: Some(RunContext {
                        run_id: "parent-run".to_string(),
                        agent_name: "parent".to_string(),
                        ..RunContext::default()
                    }),
                    sub_task_manager: Some(manager.clone()),
                    ..RuntimeRunControls::default()
                },
            )
            .expect("explicit backend configured child run");

        assert_eq!(result.status, AgentStatus::Completed, "{label}");
        assert_eq!(model_contract["explicit_backend_requires_resolver"], true);
        let refs = resolved_refs.lock().expect("resolved explicit refs");
        assert_eq!(refs.len(), 1, "{label}");
        assert_eq!(refs[0].backend_name(), Some("child-backend"), "{label}");
        assert_eq!(refs[0].model(), "child-model", "{label}");
        let requests = child_requests.lock().expect("captured child requests");
        assert!(!requests.is_empty(), "{label}");
        assert_eq!(
            requests[0].metadata["model_context_window"], expected_context,
            "{label}"
        );
        assert_eq!(
            requests[0].metadata["reserved_output_tokens"], expected_output,
            "{label}"
        );
        let run_model = inspected_model
            .lock()
            .expect("inspected model ref")
            .clone()
            .expect("child run context model");
        assert_eq!(run_model.backend_name(), Some("child-backend"), "{label}");
        assert_eq!(run_model.model(), "child-model", "{label}");
        let child_payload: Value = serde_json::from_str(&result.cycles[0].tool_results[0].content)
            .expect("configured child payload");
        let snapshot = manager
            .get(child_payload["task_id"].as_str().expect("child task id"))
            .expect("configured child snapshot");
        assert_eq!(
            snapshot.resolved.get("backend").map(String::as_str),
            Some("child-backend")
        );
        assert_eq!(
            snapshot.resolved.get("model_id").map(String::as_str),
            Some("child-model")
        );
    }

    assert_eq!(resolved_limits["explicit_child_metadata_wins"], true);
}

#[test]
fn lifecycle_starts_before_model_resolution_failure_and_remains_paired() {
    let fixture = contract();
    let lifecycle_contract = &fixture["lifecycle"];
    let ordering = Arc::new(Mutex::new(Vec::new()));
    let lifecycle = Arc::new(Mutex::new(Vec::new()));
    let ordering_for_handler = ordering.clone();
    let lifecycle_for_handler = lifecycle.clone();
    let log_handler: vv_agent::RuntimeEventHandler = Arc::new(move |name, payload| {
        if matches!(name, "sub_run_started" | "sub_run_completed") {
            ordering_for_handler
                .lock()
                .expect("lifecycle ordering")
                .push(name.to_string());
            lifecycle_for_handler
                .lock()
                .expect("resolution lifecycle")
                .push((name.to_string(), payload.clone()));
        }
    });
    let llm = ScriptedLlmClient::from_steps(vec![
        ScriptStep::response(LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::from_raw_arguments(
                "delegate",
                "create_sub_task",
                json!({"agent_id": "researcher", "task_description": "Collect facts"}),
            )],
        )),
        ScriptStep::response(LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::from_raw_arguments(
                "parent-finish",
                "task_finish",
                json!({"message": "parent handled resolution failure"}),
            )],
        )),
    ]);
    let mut parent = AgentTask::new("parent-task", "parent-model", "Parent prompt", "Delegate");
    parent.max_cycles = 2;
    let mut child = SubAgentConfig::new("child-model", "Research");
    child.backend = Some(
        fixture["model_resolution"]["blank_backend_input"]
            .as_str()
            .expect("blank backend input")
            .to_string(),
    );
    parent.sub_agents.insert("researcher".to_string(), child);
    let manager = SubTaskManager::default();
    let provider: Arc<dyn ModelProvider> = Arc::new(FailingChildModelProvider {
        ordering: ordering.clone(),
    });
    let result = AgentRuntime::new(llm)
        .run_with_controls(
            parent,
            RuntimeRunControls {
                log_handler: Some(log_handler),
                execution_context: Some(ExecutionContext {
                    metadata: BTreeMap::from([
                        ("_vv_agent_run_id".to_string(), json!("parent-run")),
                        ("_vv_agent_trace_id".to_string(), json!("trace-parity")),
                    ]),
                    ..ExecutionContext::default()
                }),
                model_provider: Some(provider),
                run_context: Some(RunContext {
                    run_id: "parent-run".to_string(),
                    agent_name: "parent".to_string(),
                    ..RunContext::default()
                }),
                sub_task_manager: Some(manager.clone()),
                ..RuntimeRunControls::default()
            },
        )
        .expect("parent handles child model resolution failure");
    assert_eq!(result.status, AgentStatus::Completed);

    let observed_ordering = ordering.lock().expect("resolution ordering").clone();
    assert_eq!(
        observed_ordering.first().map(String::as_str) == Some("sub_run_started")
            && observed_ordering.get(1).map(String::as_str) == Some("model_resolution"),
        lifecycle_contract["started_before_model_resolution"]
    );
    let lifecycle = lifecycle.lock().expect("resolution lifecycle");
    assert_eq!(
        lifecycle.iter().map(|(name, _)| name).collect::<Vec<_>>(),
        lifecycle_contract["event_sequence"]
            .as_array()
            .expect("lifecycle event sequence")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        lifecycle[0].1["child_run_id"],
        lifecycle[1].1["child_run_id"]
    );
    assert_eq!(
        lifecycle[1].1["status"],
        lifecycle_contract["resolution_failure_status"]
    );
    assert_eq!(
        lifecycle.len() == 2,
        lifecycle_contract["completed_after_every_started"]
    );
    let child_payload: Value = serde_json::from_str(&result.cycles[0].tool_results[0].content)
        .expect("resolution failure payload");
    let snapshot = manager
        .get(
            child_payload["task_id"]
                .as_str()
                .expect("failed child task id"),
        )
        .expect("failed child remains queryable");
    assert_eq!(snapshot.parent_run_id.as_deref(), Some("parent-run"));
    assert_eq!(snapshot.parent_tool_call_id.as_deref(), Some("delegate"));
}
