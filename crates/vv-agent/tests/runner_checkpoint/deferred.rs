use super::*;

async fn assert_checkpointed_started_tool_lifecycle_event_is_emitted_once(
    checkpoint_key: &str,
    tool_call_id: &str,
    result_status: ToolResultStatus,
    directive: vv_agent::ToolDirective,
    expected_status: AgentStatus,
) {
    let store = InMemoryCheckpointStore::new();
    let tool = StaticTool::new(
        "checkpointed_tool",
        "Return a checkpointed tool outcome.",
        json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false,
        }),
        Arc::new(move |context, _arguments| {
            let mut result =
                ToolExecutionResult::success(context.tool_call_id.clone(), "checkpointed");
            result.status = result_status;
            result.directive = directive;
            result
        }),
    );
    let provider = ScriptedModelProvider::new(
        "scripted",
        "checkpointed-tool-model",
        vec![LLMResponse::with_tool_calls(
            "run the checkpointed tool",
            vec![ToolCall::new(
                tool_call_id,
                "checkpointed_tool",
                BTreeMap::new(),
            )],
        )],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(".")
        .build()
        .expect("runner");
    let agent = Agent::builder("checkpointed-tool-agent")
        .instructions("Run the checkpointed tool.")
        .model(ModelRef::named("checkpointed-tool-model"))
        .tool(tool)
        .build()
        .expect("agent");

    let result = runner
        .run_with_config(
            &agent,
            "run the checkpointed tool",
            RunConfig::builder()
                .max_cycles(1)
                .no_tool_policy(NoToolPolicy::Finish)
                .checkpoint_config(checkpoint_config(store.clone(), checkpoint_key))
                .build(),
        )
        .await
        .expect("checkpointed tool run");
    assert_eq!(result.status(), expected_status);

    let completions = result
        .events()
        .iter()
        .filter(|event| {
            matches!(
                event.payload(),
                RunEventPayload::ToolCallCompleted { tool_call_id: id, .. }
                    if id == tool_call_id
            )
        })
        .count();
    assert_eq!(
        completions, 1,
        "tool lifecycle completion event must be projected once"
    );

    let persisted = store
        .load_checkpoint(checkpoint_key)
        .expect("load checkpoint")
        .expect("checkpoint");
    let outbox_completions = persisted
        .event_outbox
        .iter()
        .filter(|entry| {
            entry.event["type"] == "tool_call_completed"
                && entry.event["tool_call_id"] == tool_call_id
        })
        .collect::<Vec<_>>();
    assert_eq!(
        outbox_completions.len(),
        1,
        "durable tool receipt must own one completion event"
    );
    assert_eq!(outbox_completions[0].state, "delivered");
}

#[tokio::test]
async fn checkpointed_started_success_lifecycle_event_projects_one_completion() {
    assert_checkpointed_started_tool_lifecycle_event_is_emitted_once(
        "checkpointed-started-success",
        "call-checkpointed-success",
        ToolResultStatus::Success,
        vv_agent::ToolDirective::Continue,
        AgentStatus::MaxCycles,
    )
    .await;
}

#[tokio::test]
async fn checkpointed_started_wait_user_lifecycle_event_projects_one_completion() {
    assert_checkpointed_started_tool_lifecycle_event_is_emitted_once(
        "checkpointed-started-wait-user",
        "call-checkpointed-wait-user",
        ToolResultStatus::WaitResponse,
        vv_agent::ToolDirective::WaitUser,
        AgentStatus::WaitUser,
    )
    .await;
}

#[tokio::test]
async fn deferred_admission_projects_each_lifecycle_event_once_from_outbox() {
    let store = InMemoryCheckpointStore::new();
    let empty_schema = || {
        json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false,
        })
    };
    let deferred_tool = StaticTool::new(
        "remote_write",
        "Record a durable external write.",
        empty_schema(),
        Arc::new(|context, _arguments| {
            let _ = context.defer();
            ToolExecutionResult::success(context.tool_call_id.clone(), "not model-visible")
        }),
    );
    let success_tool = StaticTool::new(
        "ordinary_success",
        "Complete an ordinary tool call.",
        empty_schema(),
        Arc::new(|context, _arguments| {
            ToolExecutionResult::success(context.tool_call_id.clone(), "ordinary success")
        }),
    );
    let error_tool = StaticTool::new(
        "ordinary_error",
        "Return an ordinary tool error.",
        empty_schema(),
        Arc::new(|context, _arguments| {
            ToolExecutionResult::error(context.tool_call_id.clone(), "ordinary failure")
                .with_error_code("ordinary_failure")
        }),
    );
    let provider = ScriptedModelProvider::new(
        "scripted",
        "deferred-admission-model",
        vec![LLMResponse::with_tool_calls(
            "defer this write and record the ordinary outcomes",
            vec![
                ToolCall::new("call_deferred", "remote_write", BTreeMap::new()),
                ToolCall::new("call_success", "ordinary_success", BTreeMap::new()),
                ToolCall::new("call_error", "ordinary_error", BTreeMap::new()),
            ],
        )],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(".")
        .build()
        .expect("runner");
    let agent = Agent::builder("deferred-admission-agent")
        .instructions("Defer the remote write.")
        .model(ModelRef::named("deferred-admission-model"))
        .tool(deferred_tool)
        .tool(success_tool)
        .tool(error_tool)
        .build()
        .expect("agent");
    let config = RunConfig::builder()
        .max_cycles(1)
        .no_tool_policy(NoToolPolicy::Finish)
        .checkpoint_config(checkpoint_config(
            store.clone(),
            "deferred-admission-events",
        ))
        .build();

    let result = runner
        .run_with_config(&agent, "perform the write", config)
        .await
        .expect("deferred run");
    assert_eq!(result.status(), AgentStatus::Deferred);
    let events = result
        .events()
        .iter()
        .map(|event| serde_json::to_value(event).expect("event wire"))
        .collect::<Vec<_>>();
    let persisted = store
        .load_checkpoint("deferred-admission-events")
        .expect("load checkpoint")
        .expect("checkpoint");
    let deferred = events
        .iter()
        .filter(|event| event["type"] == "tool_call_deferred")
        .collect::<Vec<_>>();
    let completed = events
        .iter()
        .filter(|event| event["type"] == "tool_call_completed")
        .collect::<Vec<_>>();
    assert_eq!(deferred.len(), 1, "deferred lifecycle must be emitted once");
    assert_eq!(
        completed.len(),
        2,
        "ordinary success/error lifecycle must each be emitted once"
    );
    assert_eq!(deferred[0]["tool_call_id"], "call_deferred");
    let lifecycle_events = events
        .iter()
        .filter(|event| {
            event["type"] == "tool_call_deferred" || event["type"] == "tool_call_completed"
        })
        .collect::<Vec<_>>();
    let lifecycle_event_ids = lifecycle_events
        .iter()
        .map(|event| event["event_id"].as_str().expect("event id").to_string())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        lifecycle_event_ids.len(),
        lifecycle_events.len(),
        "lifecycle event ids must be stable and unique"
    );
    let outbox_lifecycle = persisted
        .event_outbox
        .iter()
        .filter(|entry| {
            entry.event["type"] == "tool_call_deferred"
                || entry.event["type"] == "tool_call_completed"
        })
        .collect::<Vec<_>>();
    assert_eq!(outbox_lifecycle.len(), 3);
    let outbox_event_ids = outbox_lifecycle
        .iter()
        .map(|entry| entry.event_id.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(outbox_event_ids, lifecycle_event_ids);
    assert_eq!(
        outbox_lifecycle
            .iter()
            .filter(|entry| entry.event["type"] == "tool_call_deferred")
            .count(),
        1
    );
    assert_eq!(
        outbox_lifecycle
            .iter()
            .filter(|entry| entry.event["type"] == "tool_call_completed")
            .count(),
        2
    );
}

#[tokio::test]
async fn non_definitive_tool_statuses_require_reconciliation_before_admission() {
    let store = InMemoryCheckpointStore::new();
    let schema = || {
        json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false,
        })
    };
    let deferred_tool = StaticTool::new(
        "deferred_tool",
        "Create a deferred operation.",
        schema(),
        Arc::new(|context, _arguments| {
            let _ = context.defer();
            ToolExecutionResult::success(context.tool_call_id.clone(), "deferred")
        }),
    );
    let wait_tool = StaticTool::new(
        "wait_response_tool",
        "Return a non-definitive wait response.",
        schema(),
        Arc::new(|context, _arguments| {
            let mut result = ToolExecutionResult::success(context.tool_call_id.clone(), "waiting");
            result.status = ToolResultStatus::WaitResponse;
            result
        }),
    );
    let running_tool = StaticTool::new(
        "running_tool",
        "Return a running status.",
        schema(),
        Arc::new(|context, _arguments| {
            let mut result = ToolExecutionResult::success(context.tool_call_id.clone(), "running");
            result.status = ToolResultStatus::Running;
            result
        }),
    );
    let compress_tool = StaticTool::new(
        "pending_compress_tool",
        "Return a pending-compress status.",
        schema(),
        Arc::new(|context, _arguments| {
            let mut result =
                ToolExecutionResult::success(context.tool_call_id.clone(), "pending compression");
            result.status = ToolResultStatus::PendingCompress;
            result
        }),
    );
    let provider = ScriptedModelProvider::new(
        "scripted",
        "mixed-status-model",
        vec![LLMResponse::with_tool_calls(
            "defer and preserve the other tool statuses",
            vec![
                ToolCall::new("call-deferred", "deferred_tool", BTreeMap::new()),
                ToolCall::new("call-wait", "wait_response_tool", BTreeMap::new()),
                ToolCall::new("call-running", "running_tool", BTreeMap::new()),
                ToolCall::new("call-compress", "pending_compress_tool", BTreeMap::new()),
            ],
        )],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(".")
        .build()
        .expect("runner");
    let agent = Agent::builder("mixed-status-agent")
        .instructions("Run the mixed tool batch.")
        .model(ModelRef::named("mixed-status-model"))
        .tool(deferred_tool)
        .tool(wait_tool)
        .tool(running_tool)
        .tool(compress_tool)
        .build()
        .expect("agent");
    let result = runner
        .run_with_config(
            &agent,
            "run the mixed batch",
            RunConfig::builder()
                .max_cycles(1)
                .no_tool_policy(NoToolPolicy::Finish)
                .checkpoint_config(checkpoint_config(store.clone(), "mixed-status-batch"))
                .build(),
        )
        .await
        .expect("mixed batch run");

    assert_eq!(result.status(), AgentStatus::ReconciliationRequired);
    let checkpoint = store
        .load_checkpoint("mixed-status-batch")
        .expect("load checkpoint")
        .expect("checkpoint");
    assert_eq!(checkpoint.status, CheckpointStatus::ReconciliationRequired);
    assert!(checkpoint.claim_token.is_none());
    assert!(checkpoint
        .tool_journal
        .iter()
        .any(|entry| entry.state == OperationState::Ambiguous));
    assert!(!result
        .events()
        .iter()
        .any(|event| matches!(event.payload(), RunEventPayload::ToolCallDeferred { .. })));
}

#[tokio::test]
async fn deferred_outbox_preflight_rejects_before_provider_effect() {
    let store = InMemoryCheckpointStore::new();
    let checkpoint_key = "deferred-preflight-rejection";
    let deleted = Arc::new(AtomicBool::new(false));
    let effects = Arc::new(AtomicUsize::new(0));
    let delete_store = store.clone();
    let delete_once = deleted.clone();
    let stream = Arc::new(move |event: &vv_agent::RunEvent| {
        if matches!(event.payload(), RunEventPayload::ToolCallPlanned { .. })
            && !delete_once.swap(true, Ordering::SeqCst)
        {
            delete_store
                .delete_checkpoint(checkpoint_key)
                .expect("delete checkpoint before preflight");
        }
    });
    let effects_for_tool = effects.clone();
    let deferred_tool = StaticTool::new(
        "remote_write",
        "Record a durable external write.",
        json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false,
        }),
        Arc::new(move |context, _arguments| {
            effects_for_tool.fetch_add(1, Ordering::SeqCst);
            let _ = context.defer();
            ToolExecutionResult::success(context.tool_call_id.clone(), "not model-visible")
        }),
    );
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "deferred-preflight-model",
            vec![LLMResponse::with_tool_calls(
                "defer this write",
                vec![ToolCall::new(
                    "call-preflight",
                    "remote_write",
                    BTreeMap::new(),
                )],
            )],
        ))
        .workspace(".")
        .build()
        .expect("runner");
    let agent = Agent::builder("deferred-preflight-agent")
        .instructions("Defer the remote write.")
        .model(ModelRef::named("deferred-preflight-model"))
        .tool(deferred_tool)
        .build()
        .expect("agent");
    let error = match runner
        .run_with_config(
            &agent,
            "perform the write",
            RunConfig::builder()
                .max_cycles(1)
                .no_tool_policy(NoToolPolicy::Finish)
                .stream_arc(stream)
                .checkpoint_config(checkpoint_config(store.clone(), checkpoint_key))
                .build(),
        )
        .await
    {
        Ok(_) => panic!("preflight rejection must fail the run"),
        Err(error) => error,
    };

    assert!(deleted.load(Ordering::SeqCst));
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    assert!(
        error.contains("checkpoint_not_found") || error.contains("checkpoint_store_conflict"),
        "unexpected preflight error: {error}"
    );
    assert!(
        store
            .load_checkpoint(checkpoint_key)
            .expect("load deleted checkpoint")
            .is_none(),
        "preflight rejection must not recreate the checkpoint or outbox"
    );
}

#[tokio::test]
async fn started_ambiguous_tool_emits_only_reconciliation_lifecycle() {
    let store = InMemoryCheckpointStore::new();
    let ambiguous_tool = StaticTool::new(
        "remote_write",
        "Perform a remote write.",
        json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false,
        }),
        Arc::new(|context, _arguments| {
            ToolExecutionResult::error(context.tool_call_id.clone(), "outcome is unknown")
                .with_error_code("tool_execution_failed")
        }),
    );
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "ambiguous-tool-model",
            vec![LLMResponse::with_tool_calls(
                "perform the remote write",
                vec![ToolCall::new(
                    "call-ambiguous",
                    "remote_write",
                    BTreeMap::new(),
                )],
            )],
        ))
        .workspace(".")
        .build()
        .expect("runner");
    let agent = Agent::builder("ambiguous-tool-agent")
        .instructions("Perform the remote write.")
        .model(ModelRef::named("ambiguous-tool-model"))
        .tool(ambiguous_tool)
        .build()
        .expect("agent");

    let result = runner
        .run_with_config(
            &agent,
            "perform the write",
            RunConfig::builder()
                .max_cycles(1)
                .no_tool_policy(NoToolPolicy::Finish)
                .checkpoint_config(checkpoint_config(store.clone(), "ambiguous-tool"))
                .build(),
        )
        .await
        .expect("ambiguous run");

    assert_eq!(result.status(), AgentStatus::ReconciliationRequired);
    let events = result
        .events()
        .iter()
        .map(|event| serde_json::to_value(event).expect("event wire"))
        .collect::<Vec<_>>();
    assert!(events.iter().any(|event| {
        event["type"] == "operation_ambiguous" && event["operation_id"] == "op_tool_cycle_1_call_1"
    }));
    assert!(!events.iter().any(|event| {
        event["type"] == "tool_call_completed" && event["tool_call_id"] == "call-ambiguous"
    }));
}

#[tokio::test]
async fn non_checkpoint_function_handler_error_emits_completed_lifecycle() {
    let tool = FunctionTool::builder("handler_error")
        .handler(|_context, _arguments: Value| async {
            Err::<ToolOutput, _>("handler failed".to_string())
        })
        .build()
        .expect("handler error tool");
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "handler-error-model",
            vec![LLMResponse::with_tool_calls(
                "run the failing handler",
                vec![ToolCall::new(
                    "call-handler-error",
                    "handler_error",
                    BTreeMap::new(),
                )],
            )],
        ))
        .workspace(tempfile::tempdir().expect("workspace").path())
        .build()
        .expect("runner");
    let agent = Agent::builder("handler-error-agent")
        .instructions("Run the handler.")
        .model(ModelRef::named("handler-error-model"))
        .tool(tool)
        .build()
        .expect("agent");
    let events = Arc::new(Mutex::new(Vec::<vv_agent::RunEvent>::new()));
    let observed = events.clone();
    let stream = Arc::new(move |event: &vv_agent::RunEvent| {
        observed.lock().expect("events").push(event.clone());
    });

    runner
        .run_with_config(
            &agent,
            "run the handler",
            RunConfig::builder()
                .max_cycles(1)
                .no_tool_policy(NoToolPolicy::Finish)
                .stream_arc(stream)
                .build(),
        )
        .await
        .expect("handler error run");

    let events = events.lock().expect("events");
    let completions = events
        .iter()
        .filter_map(|event| match event.payload() {
            RunEventPayload::ToolCallCompleted {
                tool_call_id,
                error_code,
                execution_started,
                ..
            } if tool_call_id == "call-handler-error" => {
                Some((error_code.as_deref(), *execution_started))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(completions, [(Some("tool_execution_failed"), true)]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_checkpoint_function_tool_timeout_emits_completed_lifecycle() {
    let tool = FunctionTool::builder("slow_handler")
        .timeout(Duration::from_millis(10))
        .handler(|_context, _arguments: Value| async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            Ok(ToolOutput::text("late"))
        })
        .build()
        .expect("slow tool");
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "timeout-model",
            vec![LLMResponse::with_tool_calls(
                "run the slow handler",
                vec![ToolCall::new(
                    "call-timeout",
                    "slow_handler",
                    BTreeMap::new(),
                )],
            )],
        ))
        .workspace(tempfile::tempdir().expect("workspace").path())
        .build()
        .expect("runner");
    let agent = Agent::builder("timeout-agent")
        .instructions("Run the slow handler.")
        .model(ModelRef::named("timeout-model"))
        .tool(tool)
        .build()
        .expect("agent");
    let events = Arc::new(Mutex::new(Vec::<vv_agent::RunEvent>::new()));
    let observed = events.clone();
    let stream = Arc::new(move |event: &vv_agent::RunEvent| {
        observed.lock().expect("events").push(event.clone());
    });

    runner
        .run_with_config(
            &agent,
            "run the slow handler",
            RunConfig::builder()
                .max_cycles(1)
                .no_tool_policy(NoToolPolicy::Finish)
                .stream_arc(stream)
                .build(),
        )
        .await
        .expect("timeout run");

    let events = events.lock().expect("events");
    let completions = events
        .iter()
        .filter_map(|event| match event.payload() {
            RunEventPayload::ToolCallCompleted {
                tool_call_id,
                error_code,
                execution_started,
                ..
            } if tool_call_id == "call-timeout" => {
                Some((error_code.as_deref(), *execution_started))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(completions, [(Some("tool_timeout"), true)]);
}

#[tokio::test]
async fn deferred_before_ambiguous_drops_the_staged_batch_fail_closed() {
    let store = InMemoryCheckpointStore::new();
    let deferred_runs = Arc::new(AtomicUsize::new(0));
    let ambiguous_runs = Arc::new(AtomicUsize::new(0));
    let deferred_runs_for_tool = deferred_runs.clone();
    let ambiguous_runs_for_tool = ambiguous_runs.clone();
    let schema = json!({
        "type": "object",
        "properties": {},
        "required": [],
        "additionalProperties": false,
    });
    let deferred_tool = StaticTool::new(
        "deferred_write",
        "Start a deferred remote write.",
        schema.clone(),
        Arc::new(move |context, _arguments| {
            deferred_runs_for_tool.fetch_add(1, Ordering::SeqCst);
            let _ = context.defer();
            ToolExecutionResult::success(context.tool_call_id.clone(), "deferred")
        }),
    );
    let ambiguous_tool = StaticTool::new(
        "ambiguous_write",
        "Return an unknown remote-write outcome.",
        schema,
        Arc::new(move |context, _arguments| {
            ambiguous_runs_for_tool.fetch_add(1, Ordering::SeqCst);
            ToolExecutionResult::error(context.tool_call_id.clone(), "outcome is unknown")
                .with_error_code("tool_execution_failed")
        }),
    );
    let provider = ScriptedModelProvider::new(
        "scripted",
        "deferred-then-ambiguous-model",
        vec![LLMResponse::with_tool_calls(
            "start the deferred write, then perform the second write",
            vec![
                ToolCall::new("call-deferred", "deferred_write", BTreeMap::new()),
                ToolCall::new("call-ambiguous", "ambiguous_write", BTreeMap::new()),
            ],
        )],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(".")
        .build()
        .expect("runner");
    let agent = Agent::builder("deferred-then-ambiguous-agent")
        .instructions("Start both remote writes.")
        .model(ModelRef::named("deferred-then-ambiguous-model"))
        .tool(deferred_tool)
        .tool(ambiguous_tool)
        .build()
        .expect("agent");
    let checkpoint_key = "deferred-then-ambiguous";
    let result = runner
        .run_with_config(
            &agent,
            "perform both writes",
            RunConfig::builder()
                .max_cycles(1)
                .no_tool_policy(NoToolPolicy::Finish)
                .checkpoint_config(checkpoint_config(store.clone(), checkpoint_key))
                .build(),
        )
        .await
        .expect("ambiguous run");

    assert_eq!(result.status(), AgentStatus::ReconciliationRequired);
    assert_eq!(deferred_runs.load(Ordering::SeqCst), 1);
    assert_eq!(ambiguous_runs.load(Ordering::SeqCst), 1);

    let checkpoint = store
        .load_checkpoint(checkpoint_key)
        .expect("load checkpoint")
        .expect("checkpoint");
    assert_eq!(checkpoint.status, CheckpointStatus::ReconciliationRequired);
    assert!(checkpoint.claim_token.is_none());
    assert!(checkpoint.claimed_cycle.is_none());
    assert!(checkpoint.lease_expires_at_ms.is_none());
    let journals = checkpoint
        .tool_journal
        .iter()
        .map(|entry| (entry.tool_call_id.clone().expect("tool call id"), entry))
        .collect::<BTreeMap<_, _>>();
    let deferred = journals.get("call-deferred").expect("deferred journal");
    assert_eq!(deferred.state, OperationState::Started);
    assert!(deferred.deferred_handle.is_none());
    assert!(deferred.result.is_none());
    assert!(deferred.error.is_none());
    let ambiguous = journals.get("call-ambiguous").expect("ambiguous journal");
    assert_eq!(ambiguous.state, OperationState::Ambiguous);
    assert!(ambiguous.deferred_handle.is_none());
    assert!(ambiguous.result.is_none());
    assert!(ambiguous.error.is_none());

    let lifecycle_outbox = checkpoint
        .event_outbox
        .iter()
        .filter(|entry| {
            entry.event["type"] == "tool_call_deferred"
                || entry.event["type"] == "tool_call_completed"
        })
        .collect::<Vec<_>>();
    assert!(lifecycle_outbox.is_empty());
    assert!(result.events().iter().all(|event| {
        !matches!(
            event.payload(),
            RunEventPayload::ToolCallDeferred { .. } | RunEventPayload::ToolCallCompleted { .. }
        )
    }));
    assert!(result
        .events()
        .iter()
        .any(|event| { matches!(event.payload(), RunEventPayload::OperationAmbiguous { .. }) }));
    assert!(result.events().iter().any(|event| {
        matches!(
            event.payload(),
            RunEventPayload::ReconciliationRequired { .. }
        )
    }));
}

#[tokio::test]
async fn completed_only_admission_rejects_invalid_success_error_code() {
    let store = InMemoryCheckpointStore::new();
    let tool = StaticTool::new(
        "invalid_success",
        "Return an invalid successful result.",
        json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false,
        }),
        Arc::new(|context, _arguments| {
            ToolExecutionResult::success(context.tool_call_id.clone(), "invalid")
                .with_error_code("unexpected_success_error")
        }),
    );
    let provider = ScriptedModelProvider::new(
        "scripted",
        "invalid-success-model",
        vec![LLMResponse::with_tool_calls(
            "return the invalid success",
            vec![ToolCall::new(
                "call-invalid-success",
                "invalid_success",
                BTreeMap::new(),
            )],
        )],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(".")
        .build()
        .expect("runner");
    let agent = Agent::builder("invalid-success-agent")
        .instructions("Run the tool.")
        .model(ModelRef::named("invalid-success-model"))
        .tool(tool)
        .build()
        .expect("agent");
    let checkpoint_key = "invalid-success-admission";
    let error = match runner
        .run_with_config(
            &agent,
            "run the tool",
            RunConfig::builder()
                .max_cycles(1)
                .no_tool_policy(NoToolPolicy::Finish)
                .checkpoint_config(checkpoint_config(store.clone(), checkpoint_key))
                .build(),
        )
        .await
    {
        Ok(_) => panic!("invalid success must fail checkpoint admission"),
        Err(error) => error,
    };
    assert!(
        error.contains("tool_result_invalid"),
        "unexpected error: {error}"
    );

    let checkpoint = store
        .load_checkpoint(checkpoint_key)
        .expect("load checkpoint")
        .expect("checkpoint");
    let journal = checkpoint
        .tool_journal
        .iter()
        .find(|entry| entry.tool_call_id.as_deref() == Some("call-invalid-success"))
        .expect("invalid success journal");
    assert_eq!(journal.state, OperationState::Started);
    assert!(checkpoint
        .event_outbox
        .iter()
        .all(|entry| entry.event["type"] != "tool_call_completed"));
}

#[tokio::test]
async fn microcompacted_deferred_resume_replays_model_and_tool_once() {
    use vv_agent::{DeferredToolHandle, MemoryWorkspaceBackend, Message, ToolCallOutcome};
    let store = InMemoryCheckpointStore::new();
    let handles = Arc::new(Mutex::new(Vec::<DeferredToolHandle>::new()));
    let observed = handles.clone();
    let tool = StaticTool::new(
        "verify",
        "Verify once.",
        json!({"type":"object","properties":{},"required":[]}),
        Arc::new(move |context, _| {
            let ToolCallOutcome::Deferred { handle } = context.defer() else {
                panic!("durable tool");
            };
            observed.lock().unwrap().push(handle);
            ToolExecutionResult::success(context.tool_call_id.clone(), "unused")
        }),
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let first_calls = calls.clone();
    let last_calls = calls.clone();
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "replay-model",
        vec![
            ScriptStep::callback(move |request| {
                first_calls.fetch_add(1, Ordering::SeqCst);
                assert!(request
                    .messages
                    .iter()
                    .any(|m| m.content.contains("<Tool Result Compact>")));
                Ok(LLMResponse::with_tool_calls(
                    "",
                    vec![ToolCall::new("verify-once", "verify", BTreeMap::new())],
                ))
            }),
            ScriptStep::callback(move |request| {
                last_calls.fetch_add(1, Ordering::SeqCst);
                assert!(request.messages.iter().any(|m| m.content == "verified"));
                Ok(LLMResponse::new("done"))
            }),
        ],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(".")
        .build()
        .unwrap();
    let agent = Agent::builder("replay")
        .instructions("Verify once, then finish.")
        .model(ModelRef::named("replay-model"))
        .tool(tool)
        .build()
        .unwrap();
    let mut messages = vec![Message::user("request")];
    for i in 0..3 {
        let id = format!("old-{i}");
        messages.push(Message {
            tool_calls: vec![ToolCall::new(&id, "search", BTreeMap::new())],
            ..Message::assistant("search")
        });
        messages.push(Message::tool("old result ".repeat(1_000), id));
    }
    let mut checkpoint = checkpoint_config(store.clone(), "microcompact-deferred");
    checkpoint.capability_refs.insert(
        "workspace".to_string(),
        CapabilityRef::new("memory", "1").unwrap(),
    );
    let config = RunConfig::builder()
        .max_cycles(2)
        .initial_messages(messages)
        .workspace_backend(Arc::new(MemoryWorkspaceBackend::default()))
        .microcompaction_policy(MicrocompactionPolicy::new(0.01, 0.005, 0, 100).unwrap())
        .checkpoint_config(checkpoint)
        .build();
    let first = runner
        .run_with_config(&agent, "request", config.clone())
        .await
        .unwrap();
    assert_eq!(first.status(), AgentStatus::Deferred);
    let handle = handles.lock().unwrap()[0].clone();
    store
        .resolve_deferred(
            handle,
            ToolExecutionResult::success("verify-once", "verified"),
        )
        .unwrap();
    let resumed = runner
        .run_with_config(&agent, "request", config)
        .await
        .unwrap();
    assert_eq!(resumed.status(), AgentStatus::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(handles.lock().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checkpointed_function_tool_timeout_remains_ambiguous_without_failed_receipt() {
    struct CaptureResult(Arc<Mutex<Vec<ToolExecutionResult>>>);
    impl vv_agent::RuntimeHook for CaptureResult {
        fn after_tool_call(
            &self,
            event: vv_agent::AfterToolCallEvent<'_>,
        ) -> Option<ToolExecutionResult> {
            self.0.lock().unwrap().push(event.result.clone());
            None
        }
    }
    let observed = Arc::new(Mutex::new(Vec::new()));
    let store = InMemoryCheckpointStore::new();
    let handler_store = store.clone();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let release_rx = Mutex::new(Some(release_rx));
    let started = Arc::new(AtomicBool::new(false));
    let handler_started = started.clone();
    let tool = FunctionTool::builder("slow")
        .timeout(Duration::from_millis(10))
        .handler(move |_context, _arguments: Value| {
            let checkpoint = handler_store
                .load_checkpoint("timeout-after-started")
                .unwrap()
                .unwrap();
            assert_eq!(checkpoint.tool_journal[0].state, OperationState::Started);
            handler_started.store(true, Ordering::SeqCst);
            let release = release_rx.lock().unwrap().take().unwrap();
            async move {
                let _ = release.await;
                Ok(ToolOutput::text("late"))
            }
        })
        .build()
        .expect("slow tool");
    let workspace = tempfile::tempdir().expect("workspace");
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "timeout-model",
            vec![LLMResponse::with_tool_calls(
                "run slow",
                vec![ToolCall::new("slow-call", "slow", BTreeMap::new())],
            )],
        ))
        .workspace(workspace.path())
        .build()
        .expect("runner");
    let agent = Agent::builder("timeout-agent")
        .instructions("Run slow.")
        .model(ModelRef::named("timeout-model"))
        .tool(tool)
        .build()
        .expect("agent");
    let mut config = checkpoint_config(store.clone(), "timeout-after-started");
    config.capability_refs.insert(
        "runtime_hook:0".to_string(),
        CapabilityRef::new("capture-timeout", "1").unwrap(),
    );
    let result = runner
        .run_with_config(
            &agent,
            "run",
            RunConfig::builder()
                .hook(Arc::new(CaptureResult(observed.clone())))
                .max_cycles(1)
                .no_tool_policy(NoToolPolicy::Finish)
                .checkpoint_config(config)
                .build(),
        )
        .await
        .expect("timeout run");
    let _ = release_tx.send(());
    assert!(started.load(Ordering::SeqCst));
    assert_eq!(result.status(), AgentStatus::ReconciliationRequired);
    let checkpoint = store
        .load_checkpoint("timeout-after-started")
        .unwrap()
        .unwrap();
    assert_eq!(checkpoint.tool_journal.len(), 1);
    let entry = &checkpoint.tool_journal[0];
    assert_eq!(entry.state, OperationState::Ambiguous);
    assert!(entry.result.is_none() && entry.error.is_none() && entry.result_digest.is_none());
    assert!(result
        .events()
        .iter()
        .any(|event| matches!(event.payload(), RunEventPayload::ToolCallStarted { .. })));
    assert!(!result
        .events()
        .iter()
        .any(|event| matches!(event.payload(), RunEventPayload::ToolCallCompleted { .. })));
    let observed = observed.lock().unwrap();
    assert_eq!(observed.len(), 1);
    assert_eq!(
        observed[0].error_code.as_deref(),
        Some("tool_timeout"),
        "{}",
        observed[0].content
    );
    assert_eq!(
        serde_json::from_str::<Value>(&observed[0].content).unwrap()["retryable"],
        json!(false)
    );
    assert_eq!(
        observed[0].metadata,
        serde_json::from_value(json!({"output_type": "error", "retryable": false})).unwrap()
    );
}
