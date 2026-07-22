use super::*;

#[derive(Clone)]
struct BlockingAsyncConfiguredClient {
    parent_calls: Arc<AtomicUsize>,
    child_started: std::sync::mpsc::Sender<()>,
    release: Arc<(Mutex<bool>, Condvar)>,
    wait_for_completion: bool,
}

impl LlmClient for BlockingAsyncConfiguredClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        let is_child = request.messages.first().is_some_and(|message| {
            message.role == vv_agent::MessageRole::System && message.content == "Child prompt"
        });
        if is_child {
            self.child_started
                .send(())
                .expect("signal async child start");
            let (released, wake) = &*self.release;
            let mut released = released.lock().expect("async child release lock");
            while !*released {
                released = wake.wait(released).expect("async child release wait");
            }
            return Ok(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "child-finish",
                    "task_finish",
                    json!({"message": "child done"}),
                )],
            ));
        }

        let parent_call = self.parent_calls.fetch_add(1, Ordering::SeqCst) + 1;
        if parent_call == 1 {
            return Ok(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "delegate",
                    "create_sub_task",
                    json!({
                        "agent_id": "researcher",
                        "task_description": "Finish after parent",
                        "wait_for_completion": self.wait_for_completion
                    }),
                )],
            ));
        }
        Ok(LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::from_raw_arguments(
                "parent-finish",
                "task_finish",
                json!({"message": "parent done"}),
            )],
        ))
    }
}

#[derive(Clone, Default)]
struct PanicThenRecoverConfiguredClient {
    parent_calls: Arc<AtomicUsize>,
    child_calls: Arc<AtomicUsize>,
}

impl LlmClient for PanicThenRecoverConfiguredClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        let is_child = request.messages.first().is_some_and(|message| {
            message.role == vv_agent::MessageRole::System && message.content == "Child prompt"
        });
        if is_child {
            let child_call = self.child_calls.fetch_add(1, Ordering::SeqCst) + 1;
            if child_call == 1 {
                panic!("configured child panicked");
            }
            return Ok(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "recovered-child-finish",
                    "task_finish",
                    json!({"message": "child recovered"}),
                )],
            ));
        }

        let parent_call = self.parent_calls.fetch_add(1, Ordering::SeqCst) + 1;
        if parent_call == 1 {
            return Ok(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "delegate",
                    "create_sub_task",
                    json!({
                        "agent_id": "researcher",
                        "task_description": "panic once",
                        "wait_for_completion": false
                    }),
                )],
            ));
        }
        Ok(LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::from_raw_arguments(
                "parent-finish",
                "task_finish",
                json!({"message": "parent done"}),
            )],
        ))
    }
}

#[test]
fn configured_async_child_panic_cleans_up_and_retained_session_can_continue() {
    let fixture = contract();
    let cleanup = &fixture["lifecycle"]["panic_or_exception_cleanup"];
    let manager = SubTaskManager::default();
    let lifecycle = Arc::new(Mutex::new(Vec::new()));
    let lifecycle_for_handler = lifecycle.clone();
    let event_handler: vv_agent::RunEventHandler = Arc::new(move |run_event| {
        let (name, payload) = typed_event_parts(run_event);
        if matches!(name.as_str(), "sub_run_started" | "sub_run_completed") {
            lifecycle_for_handler
                .lock()
                .expect("panic lifecycle")
                .push((name.to_string(), payload.clone()));
        }
    });
    let mut parent = AgentTask::new("panic-parent", "shared-model", "Parent prompt", "Delegate");
    parent.max_cycles = 3;
    let mut child = SubAgentConfig::new("shared-model", "Research");
    child.system_prompt = Some("Child prompt".to_string());
    parent.sub_agents.insert("researcher".to_string(), child);

    let parent_result = AgentRuntime::new(PanicThenRecoverConfiguredClient::default())
        .run_with_controls(
            parent,
            RuntimeRunControls {
                event_handler: Some(event_handler),
                execution_context: Some(ExecutionContext {
                    metadata: BTreeMap::from([
                        ("_vv_agent_run_id".to_string(), json!("parent-run")),
                        ("_vv_agent_trace_id".to_string(), json!("trace-parity")),
                    ]),
                    ..ExecutionContext::default()
                }),
                run_context: Some(RunContext {
                    run_id: "parent-run".to_string(),
                    agent_name: "parent".to_string(),
                    ..RunContext::default()
                }),
                sub_task_manager: Some(manager.clone()),
                ..RuntimeRunControls::default()
            },
        )
        .expect("parent returns after async child panic");
    let payload: Value = serde_json::from_str(&parent_result.cycles[0].tool_results[0].content)
        .expect("async child payload");
    let task_id = payload["task_id"].as_str().expect("child task id");
    let session_id = payload["session_id"].as_str().expect("child session id");

    assert!(manager.wait(task_id, Some(Duration::from_secs(3))));
    let failed = manager.get(task_id).expect("failed child snapshot");
    let failed_outcome = failed.outcome.as_ref().expect("failed child outcome");
    assert_eq!(failed_outcome.status, AgentStatus::Failed);
    assert_eq!(
        failed_outcome.error_code.as_deref(),
        Some("sub_task_failed")
    );
    assert_eq!(
        failed_outcome.error.as_deref(),
        Some("configured child panicked")
    );
    assert_eq!(!failed.running, cleanup["active_state_cleared"]);
    assert_eq!(
        vv_agent::get_sub_agent_session(session_id).is_none(),
        cleanup["global_session_unregistered"]
    );
    {
        let events = lifecycle.lock().expect("failed panic lifecycle");
        assert_eq!(
            events
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            vec!["sub_run_started", "sub_run_completed"]
        );
        assert_eq!(events.len() == 2, cleanup["completed_event_emitted_once"]);
        assert_eq!(events[0].1["run_id"], events[1].1["run_id"]);
        assert_eq!(events[1].1["status"], "failed");
    }

    manager
        .continue_task(task_id, "recover now")
        .expect("continue retained child session");
    assert!(manager.wait(task_id, Some(Duration::from_secs(3))));
    let recovered = manager.get(task_id).expect("recovered child snapshot");
    let recovered_outcome = recovered.outcome.as_ref().expect("recovered child outcome");
    assert_eq!(
        recovered_outcome.status == AgentStatus::Completed,
        cleanup["retained_session_can_continue"]
    );
    assert_eq!(
        recovered_outcome.final_answer.as_deref(),
        Some("child recovered")
    );
    assert_eq!(recovered.task_id, task_id);
    assert_eq!(recovered.session_id, session_id);
    assert_eq!(recovered.agent_name, "researcher");
    assert!(!recovered.running);
    assert!(vv_agent::get_sub_agent_session(session_id).is_none());
    let events = lifecycle.lock().expect("recovered panic lifecycle");
    assert_eq!(
        events
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        vec![
            "sub_run_started",
            "sub_run_completed",
            "sub_run_started",
            "sub_run_completed"
        ]
    );
    assert_eq!(events[2].1["run_id"], events[3].1["run_id"]);
    assert_ne!(events[0].1["run_id"], events[2].1["run_id"]);
    assert!(events.iter().all(|(_, event)| {
        event["task_id"] == task_id
            && event["child_session_id"] == session_id
            && event["agent_name"] == "researcher"
    }));
    assert_eq!(events[3].1["status"], "completed");
}

struct RunningAsyncConfiguredChild {
    manager: SubTaskManager,
    parent_token: vv_agent::CancellationToken,
    release: Arc<(Mutex<bool>, Condvar)>,
    lifecycle: SharedRuntimeEvents,
    task_id: String,
    session_id: String,
    parent_status: AgentStatus,
}

impl RunningAsyncConfiguredChild {
    fn release(&self) {
        let (released, wake) = &*self.release;
        *released.lock().expect("async child release lock") = true;
        wake.notify_all();
    }
}

impl Drop for RunningAsyncConfiguredChild {
    fn drop(&mut self) {
        self.release();
        let _ = self
            .manager
            .wait(&self.task_id, Some(Duration::from_secs(2)));
    }
}

struct BlockingChildRelease {
    release: Arc<(Mutex<bool>, Condvar)>,
}

impl BlockingChildRelease {
    fn release(&self) {
        let (released, wake) = &*self.release;
        *released.lock().expect("blocking child release lock") = true;
        wake.notify_all();
    }
}

impl Drop for BlockingChildRelease {
    fn drop(&mut self) {
        self.release();
    }
}

fn start_blocking_async_configured_child() -> RunningAsyncConfiguredChild {
    let (child_started_tx, child_started_rx) = std::sync::mpsc::channel();
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let llm = BlockingAsyncConfiguredClient {
        parent_calls: Arc::new(AtomicUsize::new(0)),
        child_started: child_started_tx,
        release: release.clone(),
        wait_for_completion: false,
    };
    let manager = SubTaskManager::default();
    let parent_token = vv_agent::CancellationToken::default();
    let lifecycle = Arc::new(Mutex::new(Vec::new()));
    let lifecycle_for_handler = lifecycle.clone();
    let event_handler: vv_agent::RunEventHandler = Arc::new(move |run_event| {
        let (name, payload) = typed_event_parts(run_event);
        if matches!(name.as_str(), "sub_run_started" | "sub_run_completed") {
            lifecycle_for_handler
                .lock()
                .expect("async child lifecycle")
                .push((name.to_string(), payload.clone()));
        }
    });
    let mut parent = AgentTask::new("async-parent", "shared-model", "Parent prompt", "Delegate");
    parent.max_cycles = 3;
    let mut child = SubAgentConfig::new("shared-model", "Research");
    child.system_prompt = Some("Child prompt".to_string());
    child.max_cycles = 2;
    parent.sub_agents.insert("researcher".to_string(), child);
    let result = AgentRuntime::new(llm)
        .run_with_controls(
            parent,
            RuntimeRunControls {
                event_handler: Some(event_handler),
                execution_context: Some(ExecutionContext {
                    cancellation_token: Some(parent_token.clone()),
                    metadata: BTreeMap::from([
                        ("_vv_agent_run_id".to_string(), json!("parent-run")),
                        ("_vv_agent_trace_id".to_string(), json!("trace-parity")),
                    ]),
                    ..ExecutionContext::default()
                }),
                run_context: Some(RunContext {
                    run_id: "parent-run".to_string(),
                    agent_name: "parent".to_string(),
                    ..RunContext::default()
                }),
                sub_task_manager: Some(manager.clone()),
                ..RuntimeRunControls::default()
            },
        )
        .expect("parent returns while async child runs");
    let payload: Value = serde_json::from_str(&result.cycles[0].tool_results[0].content)
        .expect("async configured child payload");
    let task_id = payload["task_id"]
        .as_str()
        .expect("async child task id")
        .to_string();
    let session_id = payload["session_id"]
        .as_str()
        .expect("async child session id")
        .to_string();
    child_started_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("real configured child reached blocking LLM");

    RunningAsyncConfiguredChild {
        manager,
        parent_token,
        release,
        lifecycle,
        task_id,
        session_id,
        parent_status: result.status,
    }
}

#[test]
fn real_async_configured_child_can_finish_after_parent_returns() {
    let fixture = contract();
    let running = start_blocking_async_configured_child();
    let snapshot = running
        .manager
        .get(&running.task_id)
        .expect("running async child snapshot");

    assert_eq!(running.parent_status, AgentStatus::Completed);
    assert!(snapshot.running);
    assert!(snapshot.outcome.is_none());
    assert_eq!(
        running.parent_status == AgentStatus::Completed && snapshot.running,
        fixture["lifecycle"]["async_may_finish_after_parent"]
    );
    assert_eq!(
        running
            .lifecycle
            .lock()
            .expect("async lifecycle before release")
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        vec!["sub_run_started"]
    );

    running.release();
    assert!(running
        .manager
        .wait(&running.task_id, Some(Duration::from_secs(2))));
    let completed = running
        .manager
        .get(&running.task_id)
        .expect("completed async child snapshot");
    assert_eq!(
        completed.outcome.as_ref().map(|outcome| outcome.status),
        Some(AgentStatus::Completed)
    );
    let lifecycle = running.lifecycle.lock().expect("async lifecycle");
    assert_eq!(
        lifecycle
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        fixture["lifecycle"]["event_sequence"]
            .as_array()
            .expect("lifecycle event sequence")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
    );
    assert_eq!(lifecycle[0].1["run_id"], lifecycle[1].1["run_id"]);
    assert!(lifecycle.iter().all(|(_, payload)| {
        payload["parent_run_id"] == "parent-run" && payload["parent_tool_call_id"] == "delegate"
    }));
}

#[test]
fn synchronous_configured_child_stays_running_and_rejects_racing_continuation() {
    let (child_started_tx, child_started_rx) = std::sync::mpsc::channel();
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let release_guard = BlockingChildRelease {
        release: release.clone(),
    };
    let llm = BlockingAsyncConfiguredClient {
        parent_calls: Arc::new(AtomicUsize::new(0)),
        child_started: child_started_tx,
        release,
        wait_for_completion: true,
    };
    let manager = SubTaskManager::default();
    let lifecycle = Arc::new(Mutex::new(Vec::<(String, BTreeMap<String, Value>)>::new()));
    let lifecycle_for_handler = lifecycle.clone();
    let event_handler: vv_agent::RunEventHandler = Arc::new(move |run_event| {
        let (name, payload) = typed_event_parts(run_event);
        if matches!(name.as_str(), "sub_run_started" | "sub_run_completed") {
            lifecycle_for_handler
                .lock()
                .expect("sync child lifecycle")
                .push((name.to_string(), payload.clone()));
        }
    });
    let mut parent = AgentTask::new("sync-parent", "shared-model", "Parent prompt", "Delegate");
    parent.max_cycles = 3;
    let mut child = SubAgentConfig::new("shared-model", "Research");
    child.system_prompt = Some("Child prompt".to_string());
    parent.sub_agents.insert("researcher".to_string(), child);
    let manager_for_run = manager.clone();
    let parent_run = std::thread::spawn(move || {
        AgentRuntime::new(llm).run_with_controls(
            parent,
            RuntimeRunControls {
                event_handler: Some(event_handler),
                sub_task_manager: Some(manager_for_run),
                ..RuntimeRunControls::default()
            },
        )
    });

    child_started_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("synchronous child reached its real LLM execution");
    let task_id = lifecycle
        .lock()
        .expect("sync child lifecycle")
        .first()
        .and_then(|(_, payload)| payload.get("task_id"))
        .and_then(Value::as_str)
        .expect("sync child task id")
        .to_string();
    let running = manager.get(&task_id).expect("running sync child snapshot");
    let globally_registered = vv_agent::get_sub_agent_session(&running.session_id).is_some();
    let continuation_error = manager
        .continue_task(&task_id, "must be rejected")
        .expect_err("racing continuation must be rejected");

    release_guard.release();
    let parent_result = parent_run
        .join()
        .expect("sync parent thread")
        .expect("sync parent result");
    let completed = manager
        .get(&task_id)
        .expect("completed sync child snapshot");

    assert!(running.running);
    assert_eq!(running.status, "running");
    assert!(globally_registered);
    assert_eq!(
        continuation_error,
        format!("Sub-task {task_id} is already running.")
    );
    assert_eq!(parent_result.status, AgentStatus::Completed);
    assert!(!completed.running);
    assert_eq!(
        completed.outcome.as_ref().map(|outcome| outcome.status),
        Some(AgentStatus::Completed)
    );
    assert!(vv_agent::get_sub_agent_session(&running.session_id).is_none());
}

#[test]
fn real_configured_child_session_self_cancel_does_not_cancel_parent() {
    let fixture = contract();
    let cancellation = &fixture["cancellation"];
    assert_eq!(
        cancellation["modes"],
        json!(["sync", "async", "batch", "continuation"])
    );
    assert_eq!(cancellation["direction"], "parent_to_child");
    let running = start_blocking_async_configured_child();
    let session = vv_agent::get_sub_agent_session(&running.session_id)
        .expect("running configured child session");

    assert!(session.cancel());
    assert_eq!(
        !running.parent_token.is_cancelled(),
        cancellation["child_does_not_cancel_parent"]
    );
    running.release();
    assert!(running
        .manager
        .wait(&running.task_id, Some(Duration::from_secs(2))));
    let completed = running
        .manager
        .get(&running.task_id)
        .expect("self-cancelled child snapshot");
    let outcome = completed.outcome.expect("self-cancelled child outcome");
    assert_eq!(
        serde_json::to_value(outcome.status).expect("serialize child status"),
        cancellation["terminal_status"]
    );
    assert!(outcome
        .error
        .as_deref()
        .is_some_and(|error| error.to_ascii_lowercase().contains("cancel")));
    assert_eq!(
        !running.parent_token.is_cancelled(),
        cancellation["child_does_not_cancel_parent"]
    );
}

#[derive(Clone, Default)]
struct PanicAfterCompletedCycleClient {
    parent_calls: Arc<AtomicUsize>,
    child_calls: Arc<AtomicUsize>,
}

impl LlmClient for PanicAfterCompletedCycleClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        if request.metadata["is_sub_task"] == json!(true) {
            let child_call = self.child_calls.fetch_add(1, Ordering::SeqCst) + 1;
            if child_call == 1 {
                let mut response = LLMResponse::new("continue after a billed cycle");
                response.token_usage = TokenUsage {
                    input_tokens: Some(13),
                    output_tokens: Some(8),
                    total_tokens: Some(21),
                    ..TokenUsage::default()
                };
                return Ok(response);
            }
            panic!("configured child panicked after one completed cycle");
        }

        let parent_call = self.parent_calls.fetch_add(1, Ordering::SeqCst) + 1;
        if parent_call == 1 {
            return Ok(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "delegate-progress",
                    "create_sub_task",
                    json!({
                        "agent_id": "researcher",
                        "task_description": "Preserve progress on panic",
                        "wait_for_completion": false
                    }),
                )],
            ));
        }
        Ok(LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::from_raw_arguments(
                "parent-progress-finish",
                "task_finish",
                json!({"message": "parent done"}),
            )],
        ))
    }
}

#[test]
fn panic_after_completed_cycle_preserves_cycles_and_token_usage() {
    let manager = SubTaskManager::default();
    let lifecycle = Arc::new(Mutex::new(Vec::<(String, BTreeMap<String, Value>)>::new()));
    let lifecycle_for_handler = lifecycle.clone();
    let event_handler: vv_agent::RunEventHandler = Arc::new(move |run_event| {
        let (name, payload) = typed_event_parts(run_event);
        if matches!(name.as_str(), "sub_run_started" | "sub_run_completed") {
            lifecycle_for_handler
                .lock()
                .expect("progress lifecycle")
                .push((name.to_string(), payload.clone()));
        }
    });
    let mut parent = AgentTask::new(
        "progress-parent",
        "shared-model",
        "Parent prompt",
        "Delegate",
    );
    parent.max_cycles = 3;
    let mut child = SubAgentConfig::new("shared-model", "Research");
    child.system_prompt = Some("Child prompt".to_string());
    child.max_cycles = 3;
    parent.sub_agents.insert("researcher".to_string(), child);

    let parent_result = AgentRuntime::new(PanicAfterCompletedCycleClient::default())
        .run_with_controls(
            parent,
            RuntimeRunControls {
                event_handler: Some(event_handler),
                sub_task_manager: Some(manager.clone()),
                ..RuntimeRunControls::default()
            },
        )
        .expect("parent returns after child progress panic");
    let payload: Value = serde_json::from_str(&parent_result.cycles[0].tool_results[0].content)
        .expect("async child payload");
    let task_id = payload["task_id"].as_str().expect("child task id");
    assert!(manager.wait(task_id, Some(Duration::from_secs(3))));

    let snapshot = manager.get(task_id).expect("progress panic snapshot");
    let outcome = snapshot.outcome.expect("progress panic outcome");
    let lifecycle = lifecycle.lock().expect("progress lifecycle");
    let completion = lifecycle
        .iter()
        .find(|(name, _)| name == "sub_run_completed")
        .map(|(_, payload)| payload)
        .expect("progress panic completion");

    assert_eq!(outcome.status, AgentStatus::Failed);
    assert_eq!(outcome.cycles, 1);
    assert_eq!(completion["status"], "failed");
    assert_eq!(completion["metadata"]["cycles"], 1);
    assert_eq!(completion["token_usage"]["input_tokens"], 13);
    assert_eq!(completion["token_usage"]["output_tokens"], 8);
    assert_eq!(completion["token_usage"]["total_tokens"], 21);
    assert_eq!(completion["token_usage"]["cycles"][0]["cycle_index"], 1);
    assert!(!snapshot.running);
    assert!(vv_agent::get_sub_agent_session(
        payload["session_id"].as_str().expect("child session id")
    )
    .is_none());
}

fn completed_async_test_outcome(request: SubTaskRequest) -> SubTaskOutcome {
    SubTaskOutcome {
        task_id: "unused".to_string(),
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
}

#[test]
fn async_single_submit_failure_uses_submit_error_code() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    context.task_id = "async-submit-parent".to_string();
    context.sub_task_manager = Some(SubTaskManager::default());
    context.sub_task_runner = Some(Arc::new(completed_async_test_outcome));

    let result = registry
        .execute(
            &ToolCall::new(
                "async-submit",
                "create_sub_task",
                BTreeMap::from([
                    ("agent_id".to_string(), json!("invalid\0thread")),
                    ("task_description".to_string(), json!("must not start")),
                    ("wait_for_completion".to_string(), json!(false)),
                ]),
            ),
            &mut context,
        )
        .expect("async submit result");
    let payload: Value = serde_json::from_str(&result.content).expect("async submit payload");

    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(result.error_code.as_deref(), Some("sub_task_submit_failed"));
    assert_eq!(payload["error_code"], "sub_task_submit_failed");
    assert!(payload["error"]
        .as_str()
        .is_some_and(|error| error.contains("thread failed to spawn")));
}

#[test]
fn async_batch_submission_failures_count_and_fail_the_overall_result() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    context.task_id = "async-batch-parent".to_string();
    context.sub_task_manager = Some(SubTaskManager::default());
    context.sub_task_runner = Some(Arc::new(completed_async_test_outcome));

    let result = registry
        .execute(
            &ToolCall::new(
                "async-batch-submit",
                "create_sub_task",
                BTreeMap::from([
                    ("agent_id".to_string(), json!("invalid\0thread")),
                    (
                        "tasks".to_string(),
                        json!([
                            {"task_description": "first spawn failure"},
                            {"task_description": "second spawn failure"}
                        ]),
                    ),
                    ("wait_for_completion".to_string(), json!(false)),
                ]),
            ),
            &mut context,
        )
        .expect("async batch result");
    let payload: Value = serde_json::from_str(&result.content).expect("async batch payload");
    let details = &payload["details"];

    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(
        result.error_code.as_deref(),
        Some("create_sub_task_batch_failed")
    );
    assert_eq!(
        details["summary"],
        json!({"total": 2, "accepted": 0, "failed": 2})
    );
    assert_eq!(details["task_ids"], json!([]));
    assert_eq!(details["results"][0]["status"], "failed");
    assert_eq!(
        details["results"][0]["error_code"],
        "sub_task_submit_failed"
    );
    assert_eq!(
        details["results"][1]["error_code"],
        "sub_task_submit_failed"
    );
}
