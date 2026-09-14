use super::*;

#[tokio::test]
async fn checkpoint_completion_sink_failure_keeps_claim_and_retries_pending_receipt() {
    assert_checkpoint_receipt_recovery(false).await;
}

#[tokio::test]
async fn checkpoint_receipt_acknowledged_before_cycle_commit_replays_once() {
    assert_checkpoint_receipt_recovery(true).await;
}

async fn assert_checkpoint_receipt_recovery(acknowledge_before_resume: bool) {
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "checkpoint-sink-model",
        vec![
            ScriptStep::callback(|_| {
                Ok(LLMResponse::with_tool_calls(
                    "write once",
                    vec![ToolCall::new(
                        "call-sink-retry",
                        "durable_write",
                        BTreeMap::new(),
                    )],
                ))
            }),
            ScriptStep::callback(|_| Ok(LLMResponse::new("done"))),
        ],
    );
    let effects = Arc::new(AtomicUsize::new(0));
    let effects_for_tool = effects.clone();
    let tool = StaticTool::new(
        "durable_write",
        "Perform one durable write.",
        json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false,
        }),
        Arc::new(move |context, _arguments| {
            effects_for_tool.fetch_add(1, Ordering::SeqCst);
            ToolExecutionResult::success(context.tool_call_id.clone(), "written")
        }),
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(tempfile::tempdir().expect("workspace").path())
        .build()
        .expect("runner");
    let agent = Agent::builder("checkpoint-sink-agent")
        .instructions("Perform the write exactly once.")
        .model(ModelRef::named("checkpoint-sink-model"))
        .tool(tool)
        .build()
        .expect("agent");
    let checkpoint_store = InMemoryCheckpointStore::new();
    let event_store = Arc::new(FailCompletionEventStore::new());
    let checkpoint_key = "checkpoint-sink-retry";
    let first = runner
        .run_with_config(
            &agent,
            "write item",
            RunConfig::builder()
                .max_cycles(2)
                .no_tool_policy(NoToolPolicy::Finish)
                .event_store(event_store.clone())
                .event_store_fail_closed(true)
                .checkpoint_config(checkpoint_config(checkpoint_store.clone(), checkpoint_key))
                .build(),
        )
        .await;
    let first_error = match first {
        Ok(_) => panic!("completion sink failure must stop the cycle"),
        Err(error) => error,
    };
    assert!(
        first_error.contains("completion sink unavailable")
            || first_error.contains("event_store_test_error"),
        "unexpected sink error: {first_error}"
    );
    assert_eq!(effects.load(Ordering::SeqCst), 1);

    let mut pending = checkpoint_store
        .load_checkpoint(checkpoint_key)
        .expect("load pending checkpoint")
        .expect("pending checkpoint");
    assert_eq!(pending.cycle_index, 0);
    assert!(
        pending.cycles.is_empty(),
        "only committed cycles enter the transcript"
    );
    assert!(!pending
        .messages
        .iter()
        .any(|message| message.role == vv_agent::MessageRole::Tool));
    assert!(
        pending.claim_token.is_some(),
        "failed delivery keeps the claim"
    );
    assert!(pending.event_outbox.iter().any(|entry| {
        entry.state == "pending"
            && entry.event["type"] == "tool_call_completed"
            && entry.event["tool_call_id"] == "call-sink-retry"
    }));
    if acknowledge_before_resume {
        for entry in pending
            .event_outbox
            .clone()
            .into_iter()
            .filter(|entry| entry.state == "pending")
        {
            let event = serde_json::from_value(entry.event).expect("durable event");
            let cursor = event_store
                .append_once(&entry.event_id, &entry.payload_digest, &event)
                .expect("deliver the receipt before the interrupted cycle commit");
            assert!(checkpoint_store
                .record_event_delivery(
                    checkpoint_key,
                    pending.claim_token.as_deref(),
                    pending.revision,
                    &entry.event_id,
                    &entry.payload_digest,
                    cursor,
                )
                .expect("acknowledge receipt"));
            pending = checkpoint_store
                .load_checkpoint(checkpoint_key)
                .unwrap()
                .unwrap();
        }
    }
    pending.lease_expires_at_ms = Some(1);
    checkpoint_store
        .save_checkpoint(pending)
        .expect("expire pending claim");

    let resumed = runner
        .run_with_config(
            &agent,
            "write item",
            RunConfig::builder()
                .max_cycles(2)
                .no_tool_policy(NoToolPolicy::Finish)
                .event_store(event_store.clone())
                .event_store_fail_closed(true)
                .checkpoint_config(checkpoint_config(checkpoint_store.clone(), checkpoint_key))
                .build(),
        )
        .await
        .expect("retry after sink recovery");
    assert_eq!(resumed.status(), AgentStatus::Completed);
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(
        resumed
            .result()
            .cycles
            .iter()
            .map(|cycle| cycle.index)
            .collect::<Vec<_>>(),
        vec![1, 2],
        "receipt replay reconstructs the uncommitted cycle exactly once"
    );
    assert_eq!(resumed.token_usage().model_calls.len(), 2);
    let persisted = checkpoint_store
        .load_checkpoint(checkpoint_key)
        .expect("load committed checkpoint")
        .expect("committed checkpoint");
    assert!(persisted.claim_token.is_none());
    assert!(!persisted.event_outbox.iter().any(|entry| {
        entry.state == "pending"
            && entry.event["type"] == "tool_call_completed"
            && entry.event["tool_call_id"] == "call-sink-retry"
    }));
    let completions = event_store
        .replay(RunEventReplayQuery::run(resumed.run_id()))
        .expect("replay durable events")
        .filter_map(Result::ok)
        .filter(|event| {
            matches!(
                event.payload(),
                RunEventPayload::ToolCallCompleted { tool_call_id, .. }
                    if tool_call_id == "call-sink-retry"
            )
        })
        .count();
    assert_eq!(completions, 1);
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
