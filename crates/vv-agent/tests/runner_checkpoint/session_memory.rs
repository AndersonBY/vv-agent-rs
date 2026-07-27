use super::*;

#[derive(Clone)]
struct FailAfterSessionMemoryReceiptStore {
    inner: InMemoryCheckpointStore,
    fail_once: Arc<AtomicBool>,
    replay_event_seen: Arc<AtomicBool>,
}

impl FailAfterSessionMemoryReceiptStore {
    fn new(inner: InMemoryCheckpointStore) -> Self {
        Self {
            inner,
            fail_once: Arc::new(AtomicBool::new(true)),
            replay_event_seen: Arc::new(AtomicBool::new(false)),
        }
    }

    fn replay_event_seen(&self) -> bool {
        self.replay_event_seen.load(Ordering::SeqCst)
    }
}

impl CheckpointStore for FailAfterSessionMemoryReceiptStore {
    fn create_checkpoint(&self, checkpoint: Checkpoint) -> Result<bool, CheckpointError> {
        self.inner.create_checkpoint(checkpoint)
    }

    fn load_checkpoint(&self, checkpoint_key: &str) -> Result<Option<Checkpoint>, CheckpointError> {
        self.inner.load_checkpoint(checkpoint_key)
    }

    fn claim_checkpoint(
        &self,
        checkpoint_key: &str,
        cycle_index: u64,
        claim_token: &str,
        lease_expires_at_ms: u64,
        now_ms: u64,
        claim_mode: ClaimMode,
    ) -> Result<Option<Checkpoint>, CheckpointError> {
        self.inner.claim_checkpoint(
            checkpoint_key,
            cycle_index,
            claim_token,
            lease_expires_at_ms,
            now_ms,
            claim_mode,
        )
    }

    fn progress_checkpoint(
        &self,
        checkpoint: Checkpoint,
        claim_token: &str,
        expected_revision: u64,
    ) -> Result<bool, CheckpointError> {
        if checkpoint
            .event_outbox
            .iter()
            .any(|entry| entry.event["type"] == "operation_replayed")
        {
            self.replay_event_seen.store(true, Ordering::SeqCst);
        }
        let session_receipt_committed = checkpoint.model_call_journal.iter().any(|entry| {
            entry.model_operation == Some(ModelCallOperation::SessionMemory)
                && entry.state == OperationState::Succeeded
        }) && checkpoint
            .model_calls
            .iter()
            .any(|record| record.operation == ModelCallOperation::SessionMemory);
        let progressed =
            self.inner
                .progress_checkpoint(checkpoint, claim_token, expected_revision)?;
        if progressed && session_receipt_committed && self.fail_once.swap(false, Ordering::SeqCst) {
            return Err(CheckpointError::new(
                "checkpoint_store_injected_failure",
                "injected failure after the session-memory receipt was persisted",
            ));
        }
        Ok(progressed)
    }

    fn suspend_checkpoint(
        &self,
        checkpoint: Checkpoint,
        claim_token: &str,
        expected_revision: u64,
    ) -> Result<bool, CheckpointError> {
        self.inner
            .suspend_checkpoint(checkpoint, claim_token, expected_revision)
    }

    fn commit_checkpoint(
        &self,
        checkpoint: Checkpoint,
        claim_token: &str,
        expected_revision: u64,
    ) -> Result<bool, CheckpointError> {
        self.inner
            .commit_checkpoint(checkpoint, claim_token, expected_revision)
    }

    fn finalize_claimed_checkpoint(
        &self,
        checkpoint: Checkpoint,
        claim_token: &str,
        expected_revision: u64,
    ) -> Result<bool, CheckpointError> {
        self.inner
            .finalize_claimed_checkpoint(checkpoint, claim_token, expected_revision)
    }

    fn finalize_checkpoint(
        &self,
        checkpoint: Checkpoint,
        expected_revision: u64,
    ) -> Result<bool, CheckpointError> {
        self.inner
            .finalize_checkpoint(checkpoint, expected_revision)
    }

    fn renew_checkpoint_claim(
        &self,
        checkpoint_key: &str,
        claim_token: &str,
        lease_expires_at_ms: u64,
        now_ms: u64,
    ) -> Result<bool, CheckpointError> {
        self.inner
            .renew_checkpoint_claim(checkpoint_key, claim_token, lease_expires_at_ms, now_ms)
    }

    fn record_event_delivery(
        &self,
        checkpoint_key: &str,
        claim_token: Option<&str>,
        expected_revision: u64,
        event_id: &str,
        payload_digest: &str,
        cursor: EventCursor,
    ) -> Result<bool, CheckpointError> {
        self.inner.record_event_delivery(
            checkpoint_key,
            claim_token,
            expected_revision,
            event_id,
            payload_digest,
            cursor,
        )
    }

    fn acknowledge_terminal(
        &self,
        checkpoint_key: &str,
        expected_revision: u64,
    ) -> Result<bool, CheckpointError> {
        self.inner
            .acknowledge_terminal(checkpoint_key, expected_revision)
    }

    fn delete_checkpoint(&self, checkpoint_key: &str) -> Result<(), CheckpointError> {
        self.inner.delete_checkpoint(checkpoint_key)
    }

    fn list_checkpoints(&self) -> Result<Vec<String>, CheckpointError> {
        self.inner.list_checkpoints()
    }
}

fn reported_usage(input_tokens: u64, output_tokens: u64) -> TokenUsage {
    TokenUsage {
        input_tokens: Some(input_tokens),
        output_tokens: Some(output_tokens),
        total_tokens: Some(input_tokens + output_tokens),
        usage_source: UsageSource::ProviderReported,
        ..TokenUsage::default()
    }
}

#[tokio::test]
async fn session_memory_receipt_replay_reapplies_state_without_rewriting_frozen_prompt() {
    let workspace = tempfile::tempdir().expect("workspace");
    let model_calls = Arc::new(AtomicUsize::new(0));
    let extraction_calls = model_calls.clone();
    let agent_calls = model_calls.clone();
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "memory-replay-model",
        vec![
            ScriptStep::callback(move |request| {
                extraction_calls.fetch_add(1, Ordering::SeqCst);
                assert!(request.tools.is_empty());
                assert_eq!(request.messages.len(), 1);
                assert!(request.messages[0]
                    .content
                    .contains("extract durable facts that should survive context compression"));
                let mut response = LLMResponse::new(
                    r#"[
                        {"category":"KEY_FACT","content":"Durable   Fact","importance":7},
                        {"category":"key_fact","content":"durable fact","importance":9}
                    ]"#,
                );
                response.token_usage = reported_usage(12, 3);
                Ok(response)
            }),
            ScriptStep::callback(move |request| {
                agent_calls.fetch_add(1, Ordering::SeqCst);
                assert!(request
                    .messages
                    .iter()
                    .all(|message| !message.content.contains("<Session Memory>")));
                let mut response = LLMResponse::new("done after memory replay");
                response.token_usage = reported_usage(20, 5);
                Ok(response)
            }),
        ],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(workspace.path())
        .build()
        .expect("runner");
    let agent = Agent::builder("memory-replay-agent")
        .instructions("Use durable memory and finish.")
        .model(ModelRef::named("memory-replay-model"))
        .build()
        .expect("agent");
    let inner_store = InMemoryCheckpointStore::new();
    let faulting_store = FailAfterSessionMemoryReceiptStore::new(inner_store.clone());
    let store_probe = faulting_store.clone();
    let session = MemorySession::new("memory-replay-session");
    let limits = RunBudgetLimits::builder()
        .max_total_tokens(1_000)
        .build()
        .expect("budget limits");
    let mut checkpoint = checkpoint_config(faulting_store, "session-memory-receipt-replay");
    checkpoint.capability_refs.insert(
        "behavior_affecting_run_metadata".to_string(),
        CapabilityRef::new("metadata.session-memory-replay", "1").expect("metadata capability"),
    );
    let config = RunConfig::builder()
        .max_cycles(1)
        .no_tool_policy(NoToolPolicy::Finish)
        .session(session)
        .session_memory_enabled(true)
        .metadata("session_id", json!("memory-replay-session"))
        .metadata("session_memory_enabled", json!(false))
        .metadata("session_memory_min_tokens", json!(1))
        .metadata("session_memory_min_text_messages", json!(1))
        .budget_limits(limits)
        .checkpoint_config(checkpoint)
        .build();
    let memory_path = workspace
        .path()
        .join(".memory/session/memory-replay-session/session_memory.json");

    let first_error = match runner
        .run_with_config(&agent, "remember this fact", config.clone())
        .await
    {
        Ok(_) => panic!("the injected checkpoint failure must interrupt the first run"),
        Err(error) => error,
    };

    assert!(
        first_error.contains("checkpoint_store_injected_failure"),
        "unexpected first-run error: {first_error}"
    );
    assert_eq!(model_calls.load(Ordering::SeqCst), 1);
    assert!(!memory_path.exists());
    let mut interrupted = inner_store
        .load_checkpoint("session-memory-receipt-replay")
        .expect("load interrupted checkpoint")
        .expect("interrupted checkpoint");
    assert_eq!(interrupted.model_calls.len(), 1);
    assert_eq!(
        interrupted.model_calls[0].operation,
        ModelCallOperation::SessionMemory
    );
    assert_eq!(interrupted.model_calls[0].usage.total_tokens, Some(15));
    assert_eq!(
        interrupted
            .budget_usage
            .as_ref()
            .and_then(|usage| usage.total_tokens),
        Some(15)
    );
    assert_eq!(
        interrupted.model_call_journal[0].state,
        OperationState::Succeeded
    );
    interrupted.lease_expires_at_ms = Some(1);
    inner_store
        .save_checkpoint(interrupted)
        .expect("expire interrupted claim");

    let resumed = runner
        .run_with_config(&agent, "remember this fact", config.clone())
        .await
        .expect("resume from the durable memory receipt");

    assert_eq!(resumed.status(), AgentStatus::Completed);
    assert_eq!(resumed.final_output(), Some("done after memory replay"));
    assert_eq!(model_calls.load(Ordering::SeqCst), 2);
    assert!(store_probe.replay_event_seen());
    assert_eq!(resumed.token_usage().model_calls.len(), 2);
    assert_eq!(
        resumed
            .token_usage()
            .model_calls
            .iter()
            .map(|record| record.operation)
            .collect::<Vec<_>>(),
        [
            ModelCallOperation::SessionMemory,
            ModelCallOperation::AgentCycle,
        ]
    );
    assert_eq!(resumed.token_usage().total_tokens, Some(40));
    assert_eq!(
        resumed.budget_usage().and_then(|usage| usage.total_tokens),
        Some(40)
    );
    let memory_before_terminal_replay = std::fs::read_to_string(&memory_path)
        .expect("session-memory state written from replayed receipt");
    let memory: Value =
        serde_json::from_str(&memory_before_terminal_replay).expect("session-memory JSON");
    let entries = memory["entries"].as_array().expect("memory entries");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["category"], "key_fact");
    assert_eq!(entries[0]["content"], "Durable   Fact");
    assert_eq!(entries[0]["importance"], 9);
    assert_eq!(memory["initialized"], true);
    assert!(memory["tokens_at_last_extraction"]
        .as_u64()
        .is_some_and(|tokens| tokens > 0));
    let terminal_before_replay = inner_store
        .load_checkpoint("session-memory-receipt-replay")
        .expect("load terminal checkpoint")
        .expect("terminal checkpoint");

    let replayed_terminal = runner
        .run_with_config(&agent, "remember this fact", config)
        .await
        .expect("terminal replay");

    assert_eq!(replayed_terminal.status(), AgentStatus::Completed);
    assert_eq!(model_calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        std::fs::read_to_string(memory_path).expect("stable session-memory state"),
        memory_before_terminal_replay
    );
    let terminal_after_replay = inner_store
        .load_checkpoint("session-memory-receipt-replay")
        .expect("load replayed terminal checkpoint")
        .expect("replayed terminal checkpoint");
    assert_eq!(
        terminal_after_replay.model_calls,
        terminal_before_replay.model_calls
    );
    assert_eq!(
        terminal_after_replay.budget_usage,
        terminal_before_replay.budget_usage
    );
    assert_eq!(
        terminal_after_replay.event_outbox,
        terminal_before_replay.event_outbox
    );
}
