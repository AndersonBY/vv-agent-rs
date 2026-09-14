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
