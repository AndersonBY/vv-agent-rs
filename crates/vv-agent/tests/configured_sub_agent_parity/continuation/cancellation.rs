use super::*;

#[derive(Clone)]
struct ContinuationCancellationClient {
    parent_calls: Arc<AtomicUsize>,
    child_calls: Arc<AtomicUsize>,
    continuation_started: std::sync::mpsc::Sender<()>,
    release: Arc<(Mutex<bool>, Condvar)>,
}

impl LlmClient for ContinuationCancellationClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        let is_child = request.messages.first().is_some_and(|message| {
            message.role == MessageRole::System && message.content == "Child prompt"
        });
        if is_child {
            let call = self.child_calls.fetch_add(1, Ordering::SeqCst) + 1;
            if call == 1 {
                return Ok(finish_response(
                    "initial-child-finish",
                    "initial child done",
                ));
            }
            self.continuation_started
                .send(())
                .expect("signal continuation start");
            let (released, wake) = &*self.release;
            let mut released = released.lock().expect("continuation release lock");
            while !*released {
                released = wake.wait(released).expect("continuation release wait");
            }
            return Ok(finish_response(
                "continued-child-finish",
                "continuation should be cancelled",
            ));
        }

        let call = self.parent_calls.fetch_add(1, Ordering::SeqCst) + 1;
        if call == 1 {
            return Ok(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "delegate",
                    "create_sub_task",
                    json!({
                        "agent_id": "researcher",
                        "task_description": "initial prompt"
                    }),
                )],
            ));
        }
        Ok(finish_response("parent-finish", "parent done"))
    }
}

#[test]
fn parent_cancellation_reaches_configured_sub_agent_continuation() {
    let fixture = contract();
    let cancellation_contract = &fixture["cancellation"];
    assert_eq!(
        cancellation_contract["modes"],
        json!(["sync", "async", "batch", "continuation"])
    );
    assert!(cancellation_contract["modes"]
        .as_array()
        .expect("cancellation modes")
        .contains(&json!("continuation")));
    assert_eq!(cancellation_contract["direction"], "parent_to_child");
    let (continuation_started_tx, continuation_started_rx) = std::sync::mpsc::channel();
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let client = ContinuationCancellationClient {
        parent_calls: Arc::new(AtomicUsize::new(0)),
        child_calls: Arc::new(AtomicUsize::new(0)),
        continuation_started: continuation_started_tx,
        release: release.clone(),
    };
    let token = vv_agent::CancellationToken::default();
    let manager = SubTaskManager::default();
    let lifecycle = Arc::new(Mutex::new(Vec::new()));
    let lifecycle_for_handler = lifecycle.clone();
    let log_handler: vv_agent::RuntimeEventHandler = Arc::new(move |name, payload| {
        if matches!(name, "sub_run_started" | "sub_run_completed") {
            lifecycle_for_handler
                .lock()
                .expect("continuation lifecycle")
                .push((name.to_string(), payload.clone()));
        }
    });
    let mut parent = AgentTask::new("parent-task", "shared-model", "Parent prompt", "Delegate");
    parent.max_cycles = 3;
    let mut child = SubAgentConfig::new("shared-model", "Research");
    child.system_prompt = Some("Child prompt".to_string());
    parent.sub_agents.insert("researcher".to_string(), child);
    let controls = RuntimeRunControls {
        log_handler: Some(log_handler),
        execution_context: Some(ExecutionContext::default().with_cancellation_token(token.clone())),
        run_context: Some(RunContext {
            run_id: "parent-run".to_string(),
            agent_name: "parent".to_string(),
            ..RunContext::default()
        }),
        sub_task_manager: Some(manager.clone()),
        ..RuntimeRunControls::default()
    };

    let result = AgentRuntime::new(client)
        .run_with_controls(parent, controls)
        .expect("initial parent run");
    let payload: Value = serde_json::from_str(&result.cycles[0].tool_results[0].content)
        .expect("initial child payload");
    let task_id = payload["task_id"].as_str().expect("child task id");
    manager
        .continue_task(task_id, "continue and block")
        .expect("start continuation");
    continuation_started_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("continuation started");
    let second_continue_error = manager
        .continue_task(task_id, "second concurrent continuation")
        .expect_err("running continuation must reject a second continue");
    assert!(second_continue_error.contains("already running"));
    assert_eq!(
        !second_continue_error.is_empty(),
        fixture["manager"]["continue_running_rejected"]
    );

    token.cancel();
    let (released, wake) = &*release;
    *released.lock().expect("continuation release lock") = true;
    wake.notify_all();
    assert!(manager.wait(task_id, Some(Duration::from_secs(3))));
    let snapshot = manager.get(task_id).expect("continued task snapshot");
    let outcome = snapshot.outcome.expect("continued task outcome");

    assert_eq!(
        serde_json::to_value(outcome.status).expect("serialize continuation status"),
        cancellation_contract["terminal_status"]
    );
    assert!(outcome
        .error
        .as_deref()
        .is_some_and(|error| error.to_ascii_lowercase().contains("cancel")));
    let lifecycle = lifecycle.lock().expect("continuation lifecycle");
    assert_eq!(
        lifecycle
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
    assert_eq!(
        lifecycle[0].1["child_run_id"],
        lifecycle[1].1["child_run_id"]
    );
    assert_eq!(
        lifecycle[2].1["child_run_id"],
        lifecycle[3].1["child_run_id"]
    );
    assert_ne!(
        lifecycle[0].1["child_run_id"],
        lifecycle[2].1["child_run_id"]
    );
    assert_eq!(lifecycle[3].1["status"], "failed");
}
