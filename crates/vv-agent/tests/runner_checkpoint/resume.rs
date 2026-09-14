use super::*;

#[path = "terminal_receipt.rs"]
mod terminal_receipt;

struct DeferThenReplayProvider {
    calls: Arc<AtomicUsize>,
}

struct FailCompletionEventStore {
    inner: vv_agent::InMemoryRunEventStore,
    failed: Arc<AtomicBool>,
}

impl FailCompletionEventStore {
    fn new() -> Self {
        Self {
            inner: vv_agent::InMemoryRunEventStore::default(),
            failed: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl RunEventStore for FailCompletionEventStore {
    fn append(&self, event: &RunEvent) -> Result<(), vv_agent::EventStoreError> {
        self.inner.append(event)
    }

    fn replay(
        &self,
        query: RunEventReplayQuery,
    ) -> Result<vv_agent::RunEventIter, vv_agent::EventStoreError> {
        self.inner.replay(query)
    }

    fn append_once(
        &self,
        event_id: &str,
        payload_digest: &str,
        event: &RunEvent,
    ) -> Result<EventCursor, vv_agent::EventStoreError> {
        if matches!(event.payload(), RunEventPayload::ToolCallCompleted { .. })
            && !self.failed.swap(true, Ordering::SeqCst)
        {
            return Err(vv_agent::EventStoreError::new(
                "event_store_test_error",
                "completion sink unavailable",
            ));
        }
        self.inner.append_once(event_id, payload_digest, event)
    }
}

impl ReconciliationProvider for DeferThenReplayProvider {
    fn reconcile(
        &self,
        _observation: &vv_agent::ResumeObservation,
    ) -> vv_agent::checkpoint::CheckpointResult<ReconciliationDecision> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(ReconciliationDecision::defer())
        } else {
            Ok(ReconciliationDecision::replay_result(
                ToolExecutionResult::success("call-defer-replay", "replayed").to_dict(),
            ))
        }
    }
}

fn defer_replay_checkpoint_config<S>(store: S, key: &str) -> CheckpointConfig
where
    S: CheckpointStore + 'static,
{
    let mut config = checkpoint_config(store, key);
    config.capability_refs.insert(
        "reconciliation_provider".to_string(),
        CapabilityRef::new("test.reconciliation.defer-replay", "1")
            .expect("reconciliation provider capability ref"),
    );
    config
}

#[derive(Clone, Default)]
struct ResumeAuthorityCostMeter {
    amount: Arc<AtomicUsize>,
    reads: Arc<AtomicUsize>,
}

impl vv_agent::HostCostMeter for ResumeAuthorityCostMeter {
    fn read(&self) -> Result<Option<vv_agent::HostCost>, String> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        vv_agent::HostCost::new("credits", self.amount.load(Ordering::SeqCst) as u64).map(Some)
    }
}

#[tokio::test]
async fn resume_authority_precedes_precancel_and_exhausted_run_start_budget() {
    for deferred in [true, false] {
        for pre_cancelled in [true, false] {
            let store = InMemoryCheckpointStore::new();
            let key = "resume-authority-before-budget";
            let model_calls = Arc::new(AtomicUsize::new(0));
            let observed_model_calls = model_calls.clone();
            let tool_calls = Arc::new(AtomicUsize::new(0));
            let observed_tool_calls = tool_calls.clone();
            let workspace = tempfile::tempdir().unwrap();
            let runner = Runner::builder()
                .model_provider(ScriptedModelProvider::from_steps(
                    "scripted",
                    "resume-authority-model",
                    vec![ScriptStep::callback(move |_| {
                        observed_model_calls.fetch_add(1, Ordering::SeqCst);
                        Ok(LLMResponse::with_tool_calls(
                            "perform the external write",
                            vec![ToolCall::new(
                                "call-external",
                                "external_write",
                                BTreeMap::new(),
                            )],
                        ))
                    })],
                ))
                .workspace(workspace.path())
                .build()
                .unwrap();
            let tool = StaticTool::new(
                "external_write",
                "Perform an external write once.",
                json!({"type": "object", "properties": {}, "additionalProperties": false}),
                Arc::new(move |context, _| {
                    observed_tool_calls.fetch_add(1, Ordering::SeqCst);
                    if deferred {
                        let _ = context.defer();
                        ToolExecutionResult::success(context.tool_call_id.clone(), "pending")
                    } else {
                        ToolExecutionResult::error(context.tool_call_id.clone(), "outcome unknown")
                            .with_error_code("tool_execution_failed")
                    }
                }),
            )
            .with_tool_metadata(ToolMetadata {
                idempotency: ToolIdempotency::Unknown,
                ..ToolMetadata::default()
            });
            let agent = Agent::builder("resume-authority-agent")
                .instructions("Perform the external write once.")
                .model(ModelRef::named("resume-authority-model"))
                .tool(tool)
                .build()
                .unwrap();
            let meter = ResumeAuthorityCostMeter::default();
            let limits = RunBudgetLimits::builder()
                .max_host_cost(vv_agent::HostCost::new("credits", 10).unwrap())
                .build()
                .unwrap();
            let mut checkpoint = checkpoint_config(store.clone(), key);
            checkpoint.ambiguous_tool_policy = vv_agent::AmbiguousToolPolicy::RequireReconciliation;
            checkpoint.capability_refs.insert(
                "host_cost_meter".to_string(),
                CapabilityRef::new("test.resume-authority-cost", "1").unwrap(),
            );
            let mut config = RunConfig::builder()
                .max_cycles(1)
                .checkpoint_config(checkpoint)
                .budget_limits(limits.clone())
                .host_cost_meter(meter.clone())
                .session_memory_enabled(false)
                .build();
            let first = runner
                .run_with_config(&agent, "write", config.clone())
                .await
                .unwrap();
            let expected = if deferred {
                AgentStatus::Deferred
            } else {
                AgentStatus::ReconciliationRequired
            };
            assert_eq!(first.status(), expected);
            let before = store.load_checkpoint(key).unwrap().unwrap();
            assert_eq!(model_calls.load(Ordering::SeqCst), 1);
            assert_eq!(tool_calls.load(Ordering::SeqCst), 1);

            if pre_cancelled {
                let token = vv_agent::CancellationToken::default();
                token.cancel();
                config.cancellation_token = Some(token);
            } else {
                meter.amount.store(20, Ordering::SeqCst);
                let mut budget = vv_agent::budget::BudgetEvaluator::new(
                    limits,
                    Some(Arc::new(meter.clone())),
                    before.budget_usage.clone(),
                )
                .unwrap();
                assert_eq!(
                    budget.run_start().unwrap().enforcement_boundary,
                    vv_agent::BudgetEnforcementBoundary::RunStart
                );
            }
            let meter_reads = meter.reads.load(Ordering::SeqCst);
            let resumed = runner
                .run_with_config(&agent, "write", config)
                .await
                .unwrap();
            assert_eq!(
                resumed.status(),
                expected,
                "deferred={deferred}, pre_cancelled={pre_cancelled}"
            );
            assert_eq!(resumed.budget_exhaustion(), None);
            assert_eq!(resumed.result().wait_reason, first.result().wait_reason);
            assert_eq!(model_calls.load(Ordering::SeqCst), 1);
            assert_eq!(tool_calls.load(Ordering::SeqCst), 1);
            assert_eq!(
                meter.reads.load(Ordering::SeqCst),
                meter_reads,
                "recovery must resolve authority before entering runtime budget checks"
            );
            let after = store.load_checkpoint(key).unwrap().unwrap();
            assert_eq!(after.status, before.status);
            assert_eq!(after.cycles, before.cycles);
            assert_eq!(after.model_calls, before.model_calls);
            assert_eq!(after.budget_usage, before.budget_usage);
            assert!(after.terminal_result.is_none());
            assert!(after.claim_token.is_none());
            assert!(!after.cancel_requested);
            assert_eq!(after.tool_journal, before.tool_journal, "unknown or deferred external work must not be retried or replaced with cancellation");
        }
    }
}

#[tokio::test]
async fn runner_recovery_surfaces_unknown_tool_outcome_by_default() {
    let model_calls = Arc::new(AtomicUsize::new(0));
    let calls_for_model = model_calls.clone();
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "checkpoint-model",
        vec![ScriptStep::callback(move |_request| {
            calls_for_model.fetch_add(1, Ordering::SeqCst);
            Ok(LLMResponse::new("must not run after ambiguous recovery"))
        })],
    );
    let tool_effects = Arc::new(AtomicUsize::new(0));
    let effects_for_tool = tool_effects.clone();
    let tool = FunctionTool::builder("unsafe_write")
        .description("A non-idempotent write used by the recovery test.")
        .tool_metadata(ToolMetadata {
            idempotency: ToolIdempotency::Unknown,
            ..ToolMetadata::default()
        })
        .handler(move |_context, _arguments: Value| {
            let effects = effects_for_tool.clone();
            async move {
                effects.fetch_add(1, Ordering::SeqCst);
                Ok(ToolOutput::text("written"))
            }
        })
        .build()
        .expect("unsafe tool");
    let workspace = tempfile::tempdir().expect("workspace");
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(workspace.path())
        .build()
        .expect("runner");
    let agent = Agent::builder("ambiguous-agent")
        .instructions("Perform the write exactly once.")
        .model(ModelRef::named("checkpoint-model"))
        .tool(tool)
        .build()
        .expect("agent");
    let store = InMemoryCheckpointStore::new();
    let session = MemorySession::new("ambiguous-session");
    let crash_once = Arc::new(AtomicBool::new(true));
    let first_crash = crash_once.clone();
    let first = runner
        .run_with_config(
            &agent,
            "write item 42",
            RunConfig::builder()
                .max_cycles(1)
                .no_tool_policy(NoToolPolicy::Finish)
                .session(session.clone())
                .checkpoint_config(checkpoint_config(store.clone(), "ambiguous-runner"))
                .before_cycle_messages(move |cycle, _messages, _state| {
                    if cycle == 1 && first_crash.swap(false, Ordering::SeqCst) {
                        panic!("deterministic crash before first model call");
                    }
                    Vec::new()
                })
                .build(),
        )
        .await;
    assert!(first.is_err());
    assert_eq!(model_calls.load(Ordering::SeqCst), 0);
    assert_eq!(tool_effects.load(Ordering::SeqCst), 0);

    let mut crashed = store
        .load_checkpoint("ambiguous-runner")
        .expect("load checkpoint")
        .expect("checkpoint");
    let arguments = serde_json::Map::from_iter([("value".to_string(), json!("42"))]);
    let idempotency_key = "idem_ambiguous_runner";
    let request_digest = tool_request_digest(
        "call-unsafe",
        "unsafe_write",
        &Value::Object(arguments.clone()),
        Some(idempotency_key),
    )
    .expect("tool request digest");
    let mut started = OperationJournalEntry::tool(
        "op_tool_cycle_1_call-unsafe",
        1,
        1,
        request_digest,
        "call-unsafe",
        "unsafe_write",
        arguments,
        Some(idempotency_key.to_string()),
        ToolIdempotency::Unknown,
    );
    started
        .transition_to(OperationState::Started)
        .expect("started operation");
    crashed.tool_journal = vec![started];
    crashed.lease_expires_at_ms = Some(1);
    store
        .save_checkpoint(crashed)
        .expect("persist ambiguous crash point");

    let resumed = runner
        .run_with_config(
            &agent,
            "write item 42",
            RunConfig::builder()
                .max_cycles(1)
                .no_tool_policy(NoToolPolicy::Finish)
                .session(session)
                .checkpoint_config(checkpoint_config(store.clone(), "ambiguous-runner"))
                .before_cycle_messages(|_cycle, _messages, _state| Vec::new())
                .build(),
        )
        .await
        .expect("reconciliation result");
    assert_eq!(resumed.status(), AgentStatus::Completed);
    assert_eq!(
        resumed.completion_reason(),
        Some(vv_agent::CompletionReason::NoToolFinish)
    );
    assert_eq!(resumed.new_items().len(), 2);
    assert_eq!(model_calls.load(Ordering::SeqCst), 1);
    assert_eq!(tool_effects.load(Ordering::SeqCst), 0);
    let completions = resumed
        .events()
        .iter()
        .filter_map(|event| match event.payload() {
            RunEventPayload::ToolCallCompleted { error_code, .. } => Some(error_code.as_deref()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(completions, vec![Some("tool_outcome_unknown")]);

    let retained = store
        .load_checkpoint("ambiguous-runner")
        .expect("load retained checkpoint")
        .expect("retained checkpoint");
    assert_eq!(retained.status, CheckpointStatus::Completed);
    assert!(retained.tool_journal.is_empty());
    assert!(retained.claim_token.is_none());
    assert!(retained.terminal_result.is_some());
}

#[tokio::test]
async fn runner_recovery_reuses_ambiguous_event_before_defer_and_replay_success() {
    let model_provider = ScriptedModelProvider::from_steps(
        "scripted",
        "defer-replay-model",
        vec![
            ScriptStep::Response(LLMResponse::with_tool_calls(
                "perform the write",
                vec![ToolCall::new(
                    "call-defer-replay",
                    "ambiguous_write",
                    BTreeMap::new(),
                )],
            )),
            ScriptStep::Response(LLMResponse::new("the write was reconciled")),
        ],
    );
    let tool = StaticTool::new(
        "ambiguous_write",
        "Perform an external write that may leave an unknown outcome.",
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
    )
    .with_tool_metadata(ToolMetadata {
        idempotency: ToolIdempotency::Unknown,
        ..ToolMetadata::default()
    });
    let runner = Runner::builder()
        .model_provider(model_provider)
        .workspace(tempfile::tempdir().expect("workspace").path())
        .build()
        .expect("runner");
    let agent = Agent::builder("defer-replay-agent")
        .instructions("Perform the write exactly once.")
        .model(ModelRef::named("defer-replay-model"))
        .tool(tool)
        .build()
        .expect("agent");
    let store = InMemoryCheckpointStore::new();
    let checkpoint_key = "defer-replay-runner";
    let reconciliation_calls = Arc::new(AtomicUsize::new(0));
    let reconciliation_provider = Arc::new(DeferThenReplayProvider {
        calls: reconciliation_calls.clone(),
    });
    let deferred = runner
        .run_with_config(
            &agent,
            "write item 42",
            RunConfig::builder()
                .max_cycles(1)
                .no_tool_policy(NoToolPolicy::Finish)
                .reconciliation_provider_arc(reconciliation_provider.clone())
                .checkpoint_config(defer_replay_checkpoint_config(
                    store.clone(),
                    checkpoint_key,
                ))
                .build(),
        )
        .await
        .expect("deferred recovery");
    assert_eq!(deferred.status(), AgentStatus::ReconciliationRequired);
    assert_eq!(reconciliation_calls.load(Ordering::SeqCst), 0);
    let suspended = store
        .load_checkpoint(checkpoint_key)
        .expect("load suspended checkpoint")
        .expect("suspended checkpoint");
    let ambiguous = suspended
        .event_outbox
        .iter()
        .filter(|entry| {
            entry.event["type"] == "operation_ambiguous"
                && entry.event["operation_id"] == "op_tool_cycle_1_call_1"
        })
        .collect::<Vec<_>>();
    assert_eq!(ambiguous.len(), 1, "first ambiguity must be durable once");
    assert_eq!(ambiguous[0].state, "delivered");
    let original_created_at = ambiguous[0].event["created_at"].clone();

    let deferred_again = runner
        .run_with_config(
            &agent,
            "write item 42",
            RunConfig::builder()
                .max_cycles(1)
                .no_tool_policy(NoToolPolicy::Finish)
                .reconciliation_provider_arc(reconciliation_provider.clone())
                .checkpoint_config(defer_replay_checkpoint_config(
                    store.clone(),
                    checkpoint_key,
                ))
                .build(),
        )
        .await
        .expect("deferred recovery");
    assert_eq!(deferred_again.status(), AgentStatus::ReconciliationRequired);
    assert_eq!(reconciliation_calls.load(Ordering::SeqCst), 1);

    let resumed = runner
        .run_with_config(
            &agent,
            "write item 42",
            RunConfig::builder()
                .max_cycles(1)
                .no_tool_policy(NoToolPolicy::Finish)
                .reconciliation_provider_arc(reconciliation_provider)
                .checkpoint_config(defer_replay_checkpoint_config(
                    store.clone(),
                    checkpoint_key,
                ))
                .build(),
        )
        .await
        .expect("replay-success recovery");
    assert_eq!(reconciliation_calls.load(Ordering::SeqCst), 2);
    assert!(resumed.events().iter().any(|event| {
        matches!(
            event.payload(),
            RunEventPayload::ReconciliationResolved {
                operation_id,
                decision: vv_agent::ReconciliationDecisionKind::ReplaySuccess,
                ..
            } if operation_id == "op_tool_cycle_1_call_1"
        )
    }));
    assert!(resumed.events().iter().any(|event| {
        matches!(
            event.payload(),
            RunEventPayload::OperationReplayed { operation_id, .. }
                if operation_id == "op_tool_cycle_1_call_1"
        )
    }));
    assert!(!resumed.events().iter().any(|event| {
        matches!(
            event.payload(),
            RunEventPayload::OperationAmbiguous { operation_id, .. }
                if operation_id == "op_tool_cycle_1_call_1"
        )
    }));
    let final_checkpoint = store
        .load_checkpoint(checkpoint_key)
        .expect("load final checkpoint")
        .expect("final checkpoint");
    let final_ambiguous = final_checkpoint
        .event_outbox
        .iter()
        .filter(|entry| {
            entry.event["type"] == "operation_ambiguous"
                && entry.event["operation_id"] == "op_tool_cycle_1_call_1"
        })
        .collect::<Vec<_>>();
    assert!(final_ambiguous.len() <= 1);
    if let Some(entry) = final_ambiguous.first() {
        assert_eq!(entry.event["created_at"], original_created_at);
    }
}

#[tokio::test]
async fn runner_recovery_projects_unknown_receipt_once_after_real_tool_panic() {
    let model_requests = Arc::new(Mutex::new(Vec::<vv_agent::LlmRequest>::new()));
    let first_requests = model_requests.clone();
    let second_requests = model_requests.clone();
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "checkpoint-model",
        vec![
            ScriptStep::callback(move |request| {
                first_requests
                    .lock()
                    .expect("first model request")
                    .push(request.clone());
                Ok(LLMResponse::with_tool_calls(
                    "perform the write",
                    vec![ToolCall::new(
                        "call-real-panic",
                        "unsafe_write",
                        BTreeMap::new(),
                    )],
                ))
            }),
            ScriptStep::callback(move |request| {
                second_requests
                    .lock()
                    .expect("recovery model request")
                    .push(request.clone());
                Ok(LLMResponse::new("the outcome is unknown"))
            }),
        ],
    );
    let tool_effects = Arc::new(AtomicUsize::new(0));
    let effects_for_tool = tool_effects.clone();
    let tool = StaticTool::new(
        "unsafe_write",
        "Perform an external write.",
        json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false,
        }),
        Arc::new(move |_context, _arguments| {
            effects_for_tool.fetch_add(1, Ordering::SeqCst);
            panic!("external write completed before the worker crashed");
        }),
    )
    .with_tool_metadata(ToolMetadata {
        idempotency: ToolIdempotency::Unknown,
        ..ToolMetadata::default()
    });
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(tempfile::tempdir().expect("workspace").path())
        .build()
        .expect("runner");
    let agent = Agent::builder("real-ambiguous-agent")
        .instructions("Perform the write exactly once.")
        .model(ModelRef::named("checkpoint-model"))
        .tool(tool)
        .build()
        .expect("agent");
    let store = InMemoryCheckpointStore::new();
    let checkpoint_key = "real-ambiguous-runner";
    let first = runner
        .run_with_config(
            &agent,
            "write item 42",
            RunConfig::builder()
                .max_cycles(2)
                .no_tool_policy(NoToolPolicy::Finish)
                .checkpoint_config(checkpoint_config(store.clone(), checkpoint_key))
                .build(),
        )
        .await;
    assert!(
        first.is_err(),
        "the real tool panic must interrupt the worker"
    );
    assert_eq!(tool_effects.load(Ordering::SeqCst), 1);

    let mut crashed = store
        .load_checkpoint(checkpoint_key)
        .expect("load crashed checkpoint")
        .expect("crashed checkpoint");
    assert_eq!(crashed.tool_journal.len(), 1);
    assert_eq!(crashed.tool_journal[0].state, OperationState::Started);
    assert!(crashed.model_call_journal[0].response.is_some());
    crashed.lease_expires_at_ms = Some(1);
    store
        .save_checkpoint(crashed)
        .expect("expire crashed claim");

    let resumed = runner
        .run_with_config(
            &agent,
            "write item 42",
            RunConfig::builder()
                .max_cycles(2)
                .no_tool_policy(NoToolPolicy::Finish)
                .checkpoint_config(checkpoint_config(store.clone(), checkpoint_key))
                .build(),
        )
        .await
        .expect("recovery run");
    assert_eq!(resumed.status(), AgentStatus::Completed);
    assert_eq!(tool_effects.load(Ordering::SeqCst), 1);

    {
        let requests = model_requests.lock().expect("model requests");
        assert_eq!(requests.len(), 2);
        let recovery_messages = &requests[1].messages;
        let assistant = recovery_messages
            .iter()
            .find(|message| {
                message.role == vv_agent::MessageRole::Assistant
                    && message
                        .tool_calls
                        .iter()
                        .any(|call| call.id == "call-real-panic")
            })
            .expect("recovery request assistant tool call");
        assert_eq!(assistant.tool_calls.len(), 1);
        let unknown_results = recovery_messages
            .iter()
            .filter(|message| {
                message.role == vv_agent::MessageRole::Tool
                    && message.tool_call_id.as_deref() == Some("call-real-panic")
                    && message.content == "The tool outcome is unknown."
            })
            .count();
        assert_eq!(
            unknown_results, 1,
            "warning transcript must be projected once"
        );
    }

    let persisted = store
        .load_checkpoint(checkpoint_key)
        .expect("load recovered checkpoint")
        .expect("recovered checkpoint");
    let durable_completions = persisted
        .event_outbox
        .iter()
        .filter(|entry| {
            entry.event["type"] == "tool_call_completed"
                && entry.event["tool_call_id"] == "call-real-panic"
        })
        .collect::<Vec<_>>();
    assert!(
        durable_completions.is_empty(),
        "delivered completion is compacted at the cycle boundary"
    );
    let public_completions = resumed
        .events()
        .iter()
        .filter(|event| {
            matches!(
                event.payload(),
                RunEventPayload::ToolCallCompleted { tool_call_id, .. }
                    if tool_call_id == "call-real-panic"
            )
        })
        .count();
    assert_eq!(
        public_completions, 1,
        "durable receipt owns public completion"
    );
    assert!(resumed.events().iter().any(|event| {
        matches!(
            event.payload(),
            RunEventPayload::ToolCallCompleted {
                tool_call_id,
                error_code,
                ..
            } if tool_call_id == "call-real-panic"
                && error_code.as_deref() == Some("tool_outcome_unknown")
        )
    }));
    assert_eq!(
        resumed
            .events()
            .iter()
            .filter(|event| {
                matches!(
                    event.payload(),
                    RunEventPayload::OperationReplayed {
                        operation_kind: OperationKind::Tool,
                        ..
                    }
                )
            })
            .count(),
        1,
        "durable tool replay must retain operation_replayed"
    );
    assert!(!resumed.events().iter().any(|event| {
        matches!(
            event.payload(),
            RunEventPayload::ToolCallPlanned { tool_call_id, .. }
                if tool_call_id == "call-real-panic"
        )
    }));

    let replay = runner
        .run_with_config(
            &agent,
            "write item 42",
            RunConfig::builder()
                .max_cycles(1)
                .no_tool_policy(NoToolPolicy::Finish)
                .checkpoint_config(checkpoint_config(store.clone(), checkpoint_key))
                .build(),
        )
        .await
        .expect("terminal replay");
    assert_eq!(model_requests.lock().expect("replayed requests").len(), 2);
    assert_eq!(
        replay
            .events()
            .iter()
            .filter(|event| {
                matches!(
                    event.payload(),
                    RunEventPayload::ToolCallCompleted { tool_call_id, .. }
                        if tool_call_id == "call-real-panic"
                )
            })
            .count(),
        0,
        "terminal replay must not project another completion"
    );
}
