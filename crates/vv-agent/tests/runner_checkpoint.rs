use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use vv_agent::{
    tool_request_digest, AfterCycleDecision, AfterCycleSnapshot, Agent, AgentStatus, CapabilityRef,
    Checkpoint, CheckpointConfig, CheckpointError, CheckpointStatus, CheckpointStore, ClaimMode,
    CycleDispatchResult, CycleDispatcher, DistributedBackend, DistributedCapabilities,
    DistributedCapabilityRegistry, DistributedCycleWorker, EventCursor, FunctionTool,
    InMemoryCheckpointStore, LLMResponse, MemorySession, ModelCallOperation, ModelRef,
    NoToolPolicy, OperationJournalEntry, OperationState, ResumePolicy, RunBudgetLimits, RunConfig,
    RunEventPayload, Runner, RuntimeRecipe, ScriptStep, ScriptedLlmClient, ScriptedModelProvider,
    Session, TokenUsage, ToolCall, ToolIdempotency, ToolMetadata, ToolOutput, UsageSource,
};

#[derive(Clone)]
struct ClaimThenFailDispatcher {
    store: InMemoryCheckpointStore,
}

impl CycleDispatcher for ClaimThenFailDispatcher {
    fn dispatch_envelope(
        &self,
        envelope: &vv_agent::DistributedRunEnvelope,
    ) -> Result<CycleDispatchResult, String> {
        let key = &envelope.checkpoint_config.key;
        let now_ms = u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_millis(),
        )
        .expect("timestamp fits u64");
        self.store
            .claim_checkpoint(
                key,
                u64::from(envelope.cycle_index),
                "external-worker-claim",
                now_ms + 60_000,
                now_ms,
                ClaimMode::Continue,
            )
            .expect("claim checkpoint")
            .expect("external worker claim");
        Err("permanent transport failure after external claim".to_string())
    }
}

#[derive(Clone)]
struct DirectWorkerDispatcher {
    worker: Arc<DistributedCycleWorker>,
    fail_after_candidate_once: Option<Arc<AtomicBool>>,
    pending_after_candidate_loss_once: Option<Arc<AtomicBool>>,
}

impl CycleDispatcher for DirectWorkerDispatcher {
    fn dispatch_envelope(
        &self,
        envelope: &vv_agent::DistributedRunEnvelope,
    ) -> Result<CycleDispatchResult, String> {
        if self
            .pending_after_candidate_loss_once
            .as_ref()
            .is_some_and(|flag| flag.swap(false, Ordering::SeqCst))
        {
            return Ok(CycleDispatchResult::pending());
        }
        let result = self.worker.run_cycle(envelope.clone())?;
        if matches!(&result, CycleDispatchResult::TerminalCandidate { .. })
            && self
                .fail_after_candidate_once
                .as_ref()
                .is_some_and(|flag| flag.swap(false, Ordering::SeqCst))
        {
            if let Some(flag) = &self.pending_after_candidate_loss_once {
                flag.store(true, Ordering::SeqCst);
            }
            return Err(
                "retryable distributed delivery conflict: candidate acknowledgement lost"
                    .to_string(),
            );
        }
        Ok(result)
    }
}

fn checkpoint_config<S>(store: S, key: &str) -> CheckpointConfig
where
    S: CheckpointStore + 'static,
{
    let mut config = CheckpointConfig::with_store(store);
    config.key = Some(key.to_string());
    config.resume_policy = ResumePolicy::ResumeIfPresent;
    config.capability_refs.insert(
        "before_cycle_messages".to_string(),
        CapabilityRef::new("runner.before-cycle", "1").expect("capability ref"),
    );
    config.capability_refs.insert(
        "session".to_string(),
        CapabilityRef::new("session.runner-checkpoint", "1").expect("capability ref"),
    );
    config
}

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
async fn run_definition_pins_after_cycle_hook_capability_slot() {
    let store = InMemoryCheckpointStore::new();
    let mut checkpoint = CheckpointConfig::with_store(store.clone());
    checkpoint.key = Some("after-cycle-definition".to_string());
    checkpoint.capability_refs.insert(
        "after_cycle_hook:0".to_string(),
        CapabilityRef::new("lifecycle.policy", "1").expect("capability ref"),
    );
    let hook =
        Arc::new(|_snapshot: &AfterCycleSnapshot| Ok(Some(AfterCycleDecision::continue_run())));
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "after-cycle-model",
            vec![LLMResponse::new("done")],
        ))
        .workspace(".")
        .build()
        .expect("runner");
    let agent = Agent::builder("after-cycle-definition-agent")
        .instructions("Answer.")
        .model(ModelRef::named("after-cycle-model"))
        .build()
        .expect("agent");
    let config = RunConfig::builder()
        .max_cycles(1)
        .no_tool_policy(NoToolPolicy::Finish)
        .after_cycle_hook_arc(hook)
        .checkpoint_config(checkpoint)
        .build();

    let result = runner
        .run_with_config(&agent, "answer", config)
        .await
        .expect("run");

    assert_eq!(result.final_output(), Some("done"));
    let stored = store
        .load_checkpoint("after-cycle-definition")
        .expect("load")
        .expect("checkpoint");
    assert_eq!(
        stored.run_definition["capability_refs"]["after_cycle_hook:0"],
        json!({"id": "lifecycle.policy", "version": "1"})
    );
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct TypedCheckpointOutput {
    answer: String,
}

#[tokio::test]
async fn terminal_replay_repeats_typed_output_validation_without_model_call() {
    let model_calls = Arc::new(AtomicUsize::new(0));
    let calls_for_model = model_calls.clone();
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "typed-checkpoint-model",
        vec![ScriptStep::callback(move |_request| {
            calls_for_model.fetch_add(1, Ordering::SeqCst);
            Ok(LLMResponse::new(r#"{"answer":42}"#))
        })],
    );
    let workspace = tempfile::tempdir().expect("workspace");
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(workspace.path())
        .build()
        .expect("runner");
    let agent = Agent::builder("typed-checkpoint-agent")
        .instructions("Return typed JSON.")
        .model(ModelRef::named("typed-checkpoint-model"))
        .output_type::<TypedCheckpointOutput>()
        .build()
        .expect("agent");
    let store = InMemoryCheckpointStore::new();
    let mut checkpoint = checkpoint_config(store.clone(), "typed-checkpoint");
    checkpoint.capability_refs.insert(
        "output_validator".to_string(),
        CapabilityRef::new("typed-checkpoint-output", "1").expect("capability ref"),
    );
    let config = RunConfig::builder()
        .max_cycles(1)
        .no_tool_policy(NoToolPolicy::Finish)
        .checkpoint_config(checkpoint)
        .build();

    let first_error = match runner
        .run_with_config(&agent, "return invalid typed output", config.clone())
        .await
    {
        Ok(_) => panic!("initial typed output validation must fail"),
        Err(error) => error,
    };
    assert!(first_error.contains("failed to validate final output"));
    assert_eq!(model_calls.load(Ordering::SeqCst), 1);
    let terminal = store
        .load_checkpoint("typed-checkpoint")
        .expect("load checkpoint")
        .expect("terminal checkpoint");
    assert_eq!(terminal.status, CheckpointStatus::Completed);
    assert!(terminal.terminal_result.is_some());

    let replay_error = match runner
        .run_with_config(&agent, "return invalid typed output", config)
        .await
    {
        Ok(_) => panic!("terminal replay must repeat typed output validation"),
        Err(error) => error,
    };
    assert!(replay_error.contains("failed to validate final output"));
    assert_eq!(model_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn session_memory_receipt_replay_reapplies_state_without_duplicate_usage() {
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
                assert!(request.messages.iter().any(|message| {
                    message.content.contains("<Session Memory>")
                        && message.content.contains("Durable   Fact")
                }));
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

#[tokio::test]
async fn distributed_worker_returns_candidate_and_runner_finalizes_once() {
    let model_calls = Arc::new(AtomicUsize::new(0));
    let model_calls_for_worker = model_calls.clone();
    let worker_llm = ScriptedLlmClient::from_steps(vec![ScriptStep::callback(move |_request| {
        model_calls_for_worker.fetch_add(1, Ordering::SeqCst);
        Ok(LLMResponse::new("done"))
    })]);
    let outer_provider = ScriptedModelProvider::new(
        "scripted",
        "distributed-checkpoint-model",
        vec![LLMResponse::new("outer provider must not execute")],
    );
    let store = InMemoryCheckpointStore::new();
    let checkpoint_ref =
        CapabilityRef::new("checkpoint.runner-distributed", "2").expect("checkpoint ref");
    let llm_ref = CapabilityRef::new("llm.runner-distributed", "1").expect("llm ref");
    let registry = DistributedCapabilityRegistry::new();
    registry.register_checkpoint_store(checkpoint_ref.clone(), Arc::new(store.clone()));
    registry.register_llm_client(llm_ref.clone(), Arc::new(worker_llm));
    let worker = Arc::new(DistributedCycleWorker::new(registry));
    let dispatcher = Arc::new(DirectWorkerDispatcher {
        worker,
        fail_after_candidate_once: None,
        pending_after_candidate_loss_once: None,
    });
    let mut recipe = RuntimeRecipe::new(
        "unused-settings.json",
        "scripted",
        "distributed-checkpoint-model",
        ".",
    );
    recipe.capabilities = DistributedCapabilities {
        llm_client_ref: Some(llm_ref),
        checkpoint_store_ref: Some(checkpoint_ref.clone()),
        ..DistributedCapabilities::default()
    };
    let backend = DistributedBackend::new(recipe, dispatcher);
    let validator_calls = Arc::new(AtomicUsize::new(0));
    let validator_calls_for_agent = validator_calls.clone();
    let agent = Agent::builder("distributed-checkpoint-agent")
        .instructions("Return done.")
        .model(ModelRef::named("distributed-checkpoint-model"))
        .output_validator("distributed-output", move |output| {
            validator_calls_for_agent.fetch_add(1, Ordering::SeqCst);
            (output == "done")
                .then_some(())
                .ok_or_else(|| "unexpected output".to_string())
        })
        .build()
        .expect("agent");
    let runner = Runner::builder()
        .model_provider(outer_provider)
        .workspace(tempfile::tempdir().expect("workspace").path())
        .build()
        .expect("runner");
    let session = MemorySession::new("distributed-checkpoint-session");
    let mut checkpoint = CheckpointConfig::with_store(store.clone());
    checkpoint.key = Some("runner-distributed-checkpoint".to_string());
    checkpoint.resume_policy = ResumePolicy::ResumeIfPresent;
    checkpoint
        .capability_refs
        .insert("checkpoint_store".to_string(), checkpoint_ref);
    checkpoint.capability_refs.insert(
        "session".to_string(),
        CapabilityRef::new("session.runner-distributed", "1").expect("session ref"),
    );
    checkpoint.capability_refs.insert(
        "output_validator".to_string(),
        CapabilityRef::new("output.runner-distributed", "1").expect("output ref"),
    );
    let config = RunConfig::builder()
        .max_cycles(1)
        .no_tool_policy(NoToolPolicy::Finish)
        .execution_backend(backend.into())
        .session(session.clone())
        .checkpoint_config(checkpoint)
        .build();

    let result = runner
        .run_with_config(&agent, "finish in the worker", config.clone())
        .await
        .expect("distributed run");

    assert_eq!(result.status(), AgentStatus::Completed);
    assert_eq!(result.final_output(), Some("done"));
    assert_eq!(model_calls.load(Ordering::SeqCst), 1);
    assert_eq!(validator_calls.load(Ordering::SeqCst), 1);
    let terminal = store
        .load_checkpoint("runner-distributed-checkpoint")
        .expect("load terminal")
        .expect("terminal checkpoint");
    assert_eq!(terminal.status, CheckpointStatus::Completed);
    assert!(terminal.claim_token.is_none());
    assert!(terminal.terminal_result.is_some());
    assert!(terminal.terminal_acknowledged);
    let session_items = session.get_items(None).await.expect("session items");
    assert!(!session_items.is_empty());

    let replay = runner
        .run_with_config(&agent, "finish in the worker", config)
        .await
        .expect("terminal replay");
    assert_eq!(replay.status(), AgentStatus::Completed);
    assert_eq!(model_calls.load(Ordering::SeqCst), 1);
    assert_eq!(validator_calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        session
            .get_items(None)
            .await
            .expect("replayed session items"),
        session_items
    );
}

#[tokio::test]
async fn distributed_candidate_ack_loss_recovers_from_receipt_without_second_model_call() {
    let model_calls = Arc::new(AtomicUsize::new(0));
    let model_calls_for_worker = model_calls.clone();
    let worker_llm = ScriptedLlmClient::from_steps(vec![ScriptStep::callback(move |_request| {
        model_calls_for_worker.fetch_add(1, Ordering::SeqCst);
        Ok(LLMResponse::new("recovered"))
    })]);
    let outer_provider = ScriptedModelProvider::new(
        "scripted",
        "candidate-recovery-model",
        vec![LLMResponse::new("outer provider must not execute")],
    );
    let store = InMemoryCheckpointStore::new();
    let checkpoint_ref =
        CapabilityRef::new("checkpoint.candidate-recovery", "2").expect("checkpoint ref");
    let llm_ref = CapabilityRef::new("llm.candidate-recovery", "1").expect("llm ref");
    let registry = DistributedCapabilityRegistry::new();
    registry.register_checkpoint_store(checkpoint_ref.clone(), Arc::new(store.clone()));
    registry.register_llm_client(llm_ref.clone(), Arc::new(worker_llm));
    let lost_ack = Arc::new(AtomicBool::new(true));
    let pending_after_loss = Arc::new(AtomicBool::new(false));
    let dispatcher = Arc::new(DirectWorkerDispatcher {
        worker: Arc::new(DistributedCycleWorker::new(registry)),
        fail_after_candidate_once: Some(lost_ack.clone()),
        pending_after_candidate_loss_once: Some(pending_after_loss.clone()),
    });
    let mut recipe = RuntimeRecipe::new(
        "unused-settings.json",
        "scripted",
        "candidate-recovery-model",
        ".",
    );
    recipe.capabilities = DistributedCapabilities {
        llm_client_ref: Some(llm_ref),
        checkpoint_store_ref: Some(checkpoint_ref.clone()),
        ..DistributedCapabilities::default()
    };
    let backend = DistributedBackend::new(recipe, dispatcher)
        .with_lease_duration(Duration::from_millis(500))
        .with_dispatch_timeout(Duration::from_secs(5));
    let agent = Agent::builder("candidate-recovery-agent")
        .instructions("Return recovered.")
        .model(ModelRef::named("candidate-recovery-model"))
        .build()
        .expect("agent");
    let runner = Runner::builder()
        .model_provider(outer_provider)
        .workspace(tempfile::tempdir().expect("workspace").path())
        .build()
        .expect("runner");
    let mut checkpoint = CheckpointConfig::with_store(store.clone());
    checkpoint.key = Some("candidate-ack-loss".to_string());
    checkpoint.resume_policy = ResumePolicy::ResumeIfPresent;
    checkpoint
        .capability_refs
        .insert("checkpoint_store".to_string(), checkpoint_ref);
    let config = RunConfig::builder()
        .max_cycles(1)
        .no_tool_policy(NoToolPolicy::Finish)
        .execution_backend(backend.into())
        .checkpoint_config(checkpoint)
        .build();

    let result = runner
        .run_with_config(&agent, "recover candidate", config)
        .await
        .expect("recovered distributed run");

    assert_eq!(result.status(), AgentStatus::Completed);
    assert_eq!(result.final_output(), Some("recovered"));
    assert_eq!(result.result().cycles.len(), 1);
    assert_eq!(model_calls.load(Ordering::SeqCst), 1);
    assert!(!lost_ack.load(Ordering::SeqCst));
    assert!(!pending_after_loss.load(Ordering::SeqCst));
    let terminal = store
        .load_checkpoint("candidate-ack-loss")
        .expect("load checkpoint")
        .expect("terminal checkpoint");
    assert_eq!(terminal.resume_attempt, 2);
    assert_eq!(terminal.status, CheckpointStatus::Completed);
    assert!(terminal.terminal_acknowledged);
}

#[tokio::test]
async fn distributed_dispatch_failure_preserves_root_error_and_external_claim() {
    let store = InMemoryCheckpointStore::new();
    let checkpoint_ref =
        CapabilityRef::new("checkpoint.dispatch-failure", "2").expect("checkpoint ref");
    let mut recipe = RuntimeRecipe::new(
        "unused-settings.json",
        "scripted",
        "dispatch-failure-model",
        ".",
    );
    recipe.capabilities = DistributedCapabilities {
        checkpoint_store_ref: Some(checkpoint_ref.clone()),
        ..DistributedCapabilities::default()
    };
    let backend = DistributedBackend::new(
        recipe,
        Arc::new(ClaimThenFailDispatcher {
            store: store.clone(),
        }),
    );
    let provider = ScriptedModelProvider::new(
        "scripted",
        "dispatch-failure-model",
        vec![LLMResponse::new("outer provider must not execute")],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(tempfile::tempdir().expect("workspace").path())
        .build()
        .expect("runner");
    let agent = Agent::builder("dispatch-failure-agent")
        .instructions("Return a result.")
        .model(ModelRef::named("dispatch-failure-model"))
        .build()
        .expect("agent");
    let mut checkpoint = CheckpointConfig::with_store(store.clone());
    checkpoint.key = Some("dispatch-failure".to_string());
    checkpoint.resume_policy = ResumePolicy::ResumeIfPresent;
    checkpoint
        .capability_refs
        .insert("checkpoint_store".to_string(), checkpoint_ref);
    let config = RunConfig::builder()
        .execution_backend(backend.into())
        .checkpoint_config(checkpoint)
        .build();

    let result = runner
        .run_with_config(&agent, "exercise dispatch failure", config)
        .await
        .expect("dispatch failure remains an observable run result");

    assert_eq!(result.status(), AgentStatus::Failed);
    assert_eq!(
        result.result().error.as_deref(),
        Some("checkpoint_dispatch_failed: permanent transport failure after external claim")
    );
    let persisted = store
        .load_checkpoint("dispatch-failure")
        .expect("load checkpoint")
        .expect("checkpoint remains durable");
    assert_eq!(persisted.status, CheckpointStatus::Running);
    assert_eq!(
        persisted.claim_token.as_deref(),
        Some("external-worker-claim")
    );
    assert!(persisted.terminal_result.is_none());
    assert!(!persisted.terminal_acknowledged);
}

#[tokio::test]
async fn distributed_execution_commits_nonterminal_cycle_before_max_cycles_candidate() {
    let model_calls = Arc::new(AtomicUsize::new(0));
    let first_calls = model_calls.clone();
    let second_calls = model_calls.clone();
    let worker_llm = ScriptedLlmClient::from_steps(vec![
        ScriptStep::callback(move |_request| {
            first_calls.fetch_add(1, Ordering::SeqCst);
            Ok(LLMResponse::new("cycle one"))
        }),
        ScriptStep::callback(move |_request| {
            second_calls.fetch_add(1, Ordering::SeqCst);
            Ok(LLMResponse::new("cycle two"))
        }),
    ]);
    let outer_provider = ScriptedModelProvider::new(
        "scripted",
        "distributed-multicycle-model",
        vec![LLMResponse::new("outer provider must not execute")],
    );
    let store = InMemoryCheckpointStore::new();
    let checkpoint_ref = CapabilityRef::new("checkpoint.distributed-multicycle", "2").unwrap();
    let llm_ref = CapabilityRef::new("llm.distributed-multicycle", "1").unwrap();
    let registry = DistributedCapabilityRegistry::new();
    registry.register_checkpoint_store(checkpoint_ref.clone(), Arc::new(store.clone()));
    registry.register_llm_client(llm_ref.clone(), Arc::new(worker_llm));
    let dispatcher = Arc::new(DirectWorkerDispatcher {
        worker: Arc::new(DistributedCycleWorker::new(registry)),
        fail_after_candidate_once: None,
        pending_after_candidate_loss_once: None,
    });
    let mut recipe = RuntimeRecipe::new(
        "unused-settings.json",
        "scripted",
        "distributed-multicycle-model",
        ".",
    );
    recipe.capabilities = DistributedCapabilities {
        llm_client_ref: Some(llm_ref),
        checkpoint_store_ref: Some(checkpoint_ref.clone()),
        ..DistributedCapabilities::default()
    };
    let backend = DistributedBackend::new(recipe, dispatcher);
    let runner = Runner::builder()
        .model_provider(outer_provider)
        .workspace(tempfile::tempdir().expect("workspace").path())
        .build()
        .expect("runner");
    let agent = Agent::builder("distributed-multicycle-agent")
        .instructions("Continue until the configured cycle budget ends.")
        .model(ModelRef::named("distributed-multicycle-model"))
        .build()
        .expect("agent");
    let mut checkpoint = CheckpointConfig::with_store(store.clone());
    checkpoint.key = Some("distributed-multicycle".to_string());
    checkpoint.resume_policy = ResumePolicy::ResumeIfPresent;
    checkpoint
        .capability_refs
        .insert("checkpoint_store".to_string(), checkpoint_ref);

    let result = runner
        .run_with_config(
            &agent,
            "run two cycles",
            RunConfig::builder()
                .max_cycles(2)
                .no_tool_policy(NoToolPolicy::Continue)
                .execution_backend(backend.into())
                .checkpoint_config(checkpoint)
                .build(),
        )
        .await
        .expect("distributed multicycle run");

    assert_eq!(result.status(), AgentStatus::MaxCycles);
    assert_eq!(result.result().cycles.len(), 2);
    assert_eq!(model_calls.load(Ordering::SeqCst), 2);
    let terminal = store
        .load_checkpoint("distributed-multicycle")
        .unwrap()
        .unwrap();
    assert_eq!(terminal.status, CheckpointStatus::MaxCycles);
    assert_eq!(terminal.cycle_index, 2);
    assert_eq!(terminal.cycles.len(), 2);
    assert!(terminal.terminal_acknowledged);
}

#[path = "runner_checkpoint/resume.rs"]
mod resume;
