use super::*;

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
async fn deferred_batch_keeps_non_definitive_tool_statuses_out_of_admission() {
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

    assert_eq!(result.status(), AgentStatus::Deferred);
    let checkpoint = store
        .load_checkpoint("mixed-status-batch")
        .expect("load checkpoint")
        .expect("checkpoint");
    assert_eq!(checkpoint.status, CheckpointStatus::Deferred);
    assert!(checkpoint.claim_token.is_none());
    let states = checkpoint
        .tool_journal
        .iter()
        .filter_map(|entry| {
            entry
                .tool_call_id
                .clone()
                .map(|tool_call_id| (tool_call_id, entry.state))
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(states["call-deferred"], OperationState::Deferred);
    assert_eq!(states["call-wait"], OperationState::Succeeded);
    assert_eq!(states["call-running"], OperationState::Failed);
    assert_eq!(states["call-compress"], OperationState::Failed);

    let lifecycle = result
        .events()
        .iter()
        .filter(|event| {
            matches!(
                event.payload(),
                RunEventPayload::ToolCallDeferred { .. }
                    | RunEventPayload::ToolCallCompleted { .. }
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        lifecycle.len(),
        4,
        "the deferred batch must not reject or duplicate non-definitive statuses"
    );
    assert_eq!(
        lifecycle
            .iter()
            .filter(|event| matches!(event.payload(), RunEventPayload::ToolCallDeferred { .. }))
            .count(),
        1
    );
    assert_eq!(
        lifecycle
            .iter()
            .filter(|event| matches!(event.payload(), RunEventPayload::ToolCallCompleted { .. }))
            .count(),
        3
    );
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
