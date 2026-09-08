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

struct CountingResumeContextProvider {
    calls: Arc<AtomicUsize>,
}

impl ContextProvider for CountingResumeContextProvider {
    fn fragments(
        &self,
        _request: &ContextRequest<'_>,
    ) -> Result<Vec<ContextFragment>, ContextError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(vec![ContextFragment::new(
            "checkpoint_context",
            "Frozen checkpoint context.",
        )
        .stable(false)
        .source("provider.checkpoint")])
    }
}

fn run_config(
    store: InMemoryCheckpointStore,
    session: MemorySession,
    crash_once: Arc<AtomicBool>,
    context_calls: Arc<AtomicUsize>,
    host_request_id: &str,
    reserved_output_tokens: u64,
) -> RunConfig {
    let mut checkpoint = checkpoint_config(store, "runner-checkpoint");
    checkpoint.capability_refs.insert(
        "behavior_affecting_run_metadata".to_string(),
        CapabilityRef::new("metadata.request-42", "1").expect("run metadata capability ref"),
    );
    checkpoint.capability_refs.insert(
        "agent.instructions".to_string(),
        CapabilityRef::new("agent.dynamic-prompt-bundle", "1")
            .expect("dynamic instructions capability ref"),
    );
    checkpoint.capability_refs.insert(
        "context_provider:0".to_string(),
        CapabilityRef::new("context.checkpoint", "1").expect("context capability ref"),
    );
    RunConfig::builder()
        .max_cycles(2)
        .no_tool_policy(NoToolPolicy::Finish)
        .metadata("host_request_id", json!(host_request_id))
        .metadata("reserved_output_tokens", json!(reserved_output_tokens))
        .session(session)
        .context_provider(Arc::new(CountingResumeContextProvider {
            calls: context_calls,
        }))
        .checkpoint_config(checkpoint)
        .before_cycle_messages(move |cycle, _messages, _state| {
            if cycle == 2 && crash_once.swap(false, Ordering::SeqCst) {
                panic!("deterministic crash after committed cycle");
            }
            Vec::new()
        })
        .build()
}

#[tokio::test]
async fn runner_resumes_committed_state_and_terminal_replay_is_side_effect_free() {
    for idempotency in [
        ToolIdempotency::Supported,
        ToolIdempotency::Unsupported,
        ToolIdempotency::Unknown,
    ] {
        assert_committed_resume_and_terminal_replay(idempotency).await;
    }
}

async fn assert_committed_resume_and_terminal_replay(idempotency: ToolIdempotency) {
    let model_calls = Arc::new(AtomicUsize::new(0));
    let model_metadata = Arc::new(Mutex::new(Vec::<Value>::new()));
    let model_prompt_bundles = Arc::new(Mutex::new(Vec::<PromptBundle>::new()));
    let first_calls = model_calls.clone();
    let second_calls = model_calls.clone();
    let first_metadata = model_metadata.clone();
    let second_metadata = model_metadata.clone();
    let first_bundles = Arc::clone(&model_prompt_bundles);
    let second_bundles = Arc::clone(&model_prompt_bundles);
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "checkpoint-model",
        vec![
            ScriptStep::callback(move |request| {
                first_calls.fetch_add(1, Ordering::SeqCst);
                first_metadata
                    .lock()
                    .expect("first model metadata")
                    .push(request.metadata.clone());
                first_bundles
                    .lock()
                    .expect("first model prompt bundle")
                    .push(request.prompt_bundle.clone());
                Ok(LLMResponse::with_tool_calls(
                    "write once",
                    vec![ToolCall::new("call-write", "write_record", BTreeMap::new())],
                ))
            }),
            ScriptStep::callback(move |request| {
                second_calls.fetch_add(1, Ordering::SeqCst);
                second_metadata
                    .lock()
                    .expect("second model metadata")
                    .push(request.metadata.clone());
                second_bundles
                    .lock()
                    .expect("second model prompt bundle")
                    .push(request.prompt_bundle.clone());
                Ok(LLMResponse::new("done"))
            }),
        ],
    );
    let observed_keys = Arc::new(Mutex::new(Vec::<Option<String>>::new()));
    let keys_for_tool = observed_keys.clone();
    let tool = FunctionTool::builder("write_record")
        .description("Record one idempotent side effect.")
        .json_schema(json!({
            "type": "object",
            "properties": {},
            "required": []
        }))
        .tool_metadata(ToolMetadata {
            idempotency,
            ..ToolMetadata::default()
        })
        .handler(move |context, _arguments: Value| {
            let keys = keys_for_tool.clone();
            async move {
                keys.lock()
                    .expect("idempotency keys")
                    .push(context.idempotency_key);
                Ok(ToolOutput::text("written"))
            }
        })
        .build()
        .expect("tool");
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(tempfile::tempdir().expect("workspace").path())
        .build()
        .expect("runner");
    let instruction_calls = Arc::new(AtomicUsize::new(0));
    let clock_calls = Arc::new(AtomicUsize::new(0));
    let instruction_calls_for_agent = Arc::clone(&instruction_calls);
    let clock_calls_for_agent = Arc::clone(&clock_calls);
    let agent = Agent::builder("checkpoint-agent")
        .dynamic_prompt_bundle(move |_context, _agent| {
            instruction_calls_for_agent.fetch_add(1, Ordering::SeqCst);
            let clock_index = clock_calls_for_agent.fetch_add(1, Ordering::SeqCst);
            PromptBundle::new(vec![
                PromptSection::new(
                    "checkpoint_instructions",
                    "Write the record, then return the final answer.",
                    true,
                )
                .source("agent.instructions"),
                PromptSection::new(
                    "current_time",
                    format!("2026-07-25T00:00:0{clock_index}Z"),
                    false,
                )
                .source("run.clock"),
            ])
            .expect("dynamic prompt bundle")
        })
        .model(ModelRef::named("checkpoint-model"))
        .tool(tool)
        .build()
        .expect("agent");
    let store = InMemoryCheckpointStore::new();
    let session = MemorySession::new("runner-checkpoint-session");
    let crash_once = Arc::new(AtomicBool::new(true));
    let context_calls = Arc::new(AtomicUsize::new(0));

    let first = runner
        .run_with_config(
            &agent,
            "process item 42",
            run_config(
                store.clone(),
                session.clone(),
                crash_once.clone(),
                Arc::clone(&context_calls),
                "request-42",
                4_096,
            ),
        )
        .await;
    let first_error = match first {
        Ok(_) => panic!("first run must crash"),
        Err(error) => error,
    };
    assert!(
        first_error.contains("runner task failed"),
        "spawn-blocking panic must surface to the caller"
    );
    assert_eq!(model_calls.load(Ordering::SeqCst), 1);
    assert_eq!(instruction_calls.load(Ordering::SeqCst), 1);
    assert_eq!(context_calls.load(Ordering::SeqCst), 1);
    assert_eq!(clock_calls.load(Ordering::SeqCst), 1);
    let keys = observed_keys.lock().expect("idempotency keys").clone();
    assert_eq!(keys.len(), 1);
    if idempotency == ToolIdempotency::Unsupported {
        assert!(keys[0].is_none());
    } else {
        assert!(keys[0]
            .as_ref()
            .expect("stable idempotency key")
            .starts_with("idem_"));
    }

    let mut crashed = store
        .load_checkpoint("runner-checkpoint")
        .expect("load crashed checkpoint")
        .expect("crashed checkpoint");
    assert_eq!(crashed.cycle_index, 1);
    assert_eq!(crashed.cycles.len(), 1);
    assert_eq!(crashed.resume_attempt, 1);
    assert!(crashed.claim_token.is_some());
    assert_eq!(
        crashed.run_definition["run_metadata"]["host_request_id"],
        "request-42"
    );
    assert_eq!(
        crashed.run_definition["run_metadata"]["reserved_output_tokens"],
        4_096
    );
    let frozen_prompt_bundle = PromptBundle::from_value(&crashed.run_definition["prompt_bundle"])
        .expect("frozen prompt bundle");
    assert_eq!(
        model_prompt_bundles
            .lock()
            .expect("first model prompt bundle")[0],
        frozen_prompt_bundle
    );
    let original_run_id = crashed.root_run_id.clone();
    let original_trace_id = crashed.trace_id.clone();
    assert!(!crashed.messages[0].metadata.is_empty());
    crashed.messages[0].metadata.clear();
    crashed.lease_expires_at_ms = Some(1);
    store
        .save_checkpoint(crashed)
        .expect("expire crashed claim");

    let resumed = runner
        .run_with_config(
            &agent,
            "process item 42",
            run_config(
                store.clone(),
                session.clone(),
                crash_once.clone(),
                Arc::clone(&context_calls),
                "stale-request",
                1_024,
            ),
        )
        .await
        .expect("resume");
    assert_eq!(resumed.status(), AgentStatus::Completed);
    assert_eq!(resumed.final_output(), Some("done"));
    assert_eq!(resumed.run_id(), original_run_id);
    assert_eq!(resumed.trace_id(), original_trace_id);
    assert_eq!(resumed.result().cycles.len(), 2);
    assert_eq!(model_calls.load(Ordering::SeqCst), 2);
    assert_eq!(instruction_calls.load(Ordering::SeqCst), 1);
    assert_eq!(context_calls.load(Ordering::SeqCst), 1);
    assert_eq!(clock_calls.load(Ordering::SeqCst), 1);
    assert_eq!(observed_keys.lock().expect("idempotency keys").len(), 1);
    {
        let observed_metadata = model_metadata.lock().expect("model metadata");
        assert_eq!(observed_metadata.len(), 2);
        assert_eq!(observed_metadata[1]["host_request_id"], "request-42");
        assert_eq!(observed_metadata[1]["reserved_output_tokens"], 4_096);
        assert!(observed_metadata
            .iter()
            .all(|metadata| metadata.get("system_prompt_sections").is_none()));
    }
    assert!(model_prompt_bundles
        .lock()
        .expect("model prompt bundles")
        .iter()
        .all(|bundle| bundle == &frozen_prompt_bundle));

    let terminal = store
        .load_checkpoint("runner-checkpoint")
        .expect("load terminal")
        .expect("terminal checkpoint");
    assert_eq!(terminal.resume_attempt, 2);
    assert!(terminal.terminal_result.is_some());
    assert!(terminal.terminal_acknowledged);
    let persisted_items = session.get_items(None).await.expect("session items");
    assert!(!persisted_items.is_empty());

    let replay = runner
        .run_with_config(
            &agent,
            "process item 42",
            run_config(
                store.clone(),
                session.clone(),
                crash_once,
                Arc::clone(&context_calls),
                "newer-stale-request",
                512,
            ),
        )
        .await
        .expect("terminal replay");
    assert_eq!(replay.status(), AgentStatus::Completed);
    assert_eq!(replay.final_output(), Some("done"));
    assert_eq!(replay.run_id(), original_run_id);
    assert_eq!(replay.trace_id(), original_trace_id);
    assert_eq!(model_calls.load(Ordering::SeqCst), 2);
    assert_eq!(instruction_calls.load(Ordering::SeqCst), 1);
    assert_eq!(context_calls.load(Ordering::SeqCst), 1);
    assert_eq!(clock_calls.load(Ordering::SeqCst), 1);
    assert_eq!(observed_keys.lock().expect("idempotency keys").len(), 1);
    assert_eq!(
        session
            .get_items(None)
            .await
            .expect("replayed session items"),
        persisted_items
    );
    assert!(!replay.events().iter().any(|event| matches!(
        event.payload(),
        RunEventPayload::RunCompleted { .. }
            | RunEventPayload::RunFailed { .. }
            | RunEventPayload::RunCancelled { .. }
    )));
}
