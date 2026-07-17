use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use vv_agent::{
    tool_request_digest, Agent, AgentStatus, CapabilityRef, CheckpointConfig, CheckpointStatus,
    CheckpointStoreV2, ClaimMode, CycleDispatchResult, CycleDispatcher, DistributedBackend,
    DistributedCapabilities, DistributedCapabilityRegistry, DistributedCycleWorker, FunctionTool,
    InMemoryCheckpointStoreV2, InMemoryStateStore, LLMResponse, MemorySession, ModelRef,
    NoToolPolicy, OperationJournalEntry, OperationState, ResumePolicy, RunConfig, RunEventPayload,
    Runner, RuntimeRecipe, ScriptStep, ScriptedLlmClient, ScriptedModelProvider, Session, ToolCall,
    ToolIdempotency, ToolOutput,
};

#[derive(Clone)]
struct ClaimThenFailDispatcher {
    store: InMemoryCheckpointStoreV2,
}

impl CycleDispatcher for ClaimThenFailDispatcher {
    fn dispatch_cycle(
        &self,
        _task: &vv_agent::AgentTask,
        _recipe: &RuntimeRecipe,
        _cycle_name: &str,
        _cycle_index: u32,
    ) -> Result<CycleDispatchResult, String> {
        Err("ClaimThenFailDispatcher requires a checkpoint v2 envelope".to_string())
    }

    fn dispatch_envelope(
        &self,
        envelope: &vv_agent::DistributedRunEnvelope,
    ) -> Result<CycleDispatchResult, String> {
        let key = &envelope
            .checkpoint_config
            .as_ref()
            .expect("checkpoint config")
            .key;
        let now_ms = u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_millis(),
        )
        .expect("timestamp fits u64");
        self.store
            .claim_checkpoint_v2(
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
    fn dispatch_cycle(
        &self,
        task: &vv_agent::AgentTask,
        recipe: &RuntimeRecipe,
        cycle_name: &str,
        cycle_index: u32,
    ) -> Result<CycleDispatchResult, String> {
        let envelope = vv_agent::DistributedRunEnvelope::for_cycle(
            task.clone(),
            recipe.clone(),
            cycle_index,
            cycle_name,
            None,
            None,
            5 * 60 * 1_000,
            None,
        )?;
        self.worker.run_cycle(envelope)
    }

    fn dispatch_envelope(
        &self,
        envelope: &vv_agent::DistributedRunEnvelope,
    ) -> Result<CycleDispatchResult, String> {
        if self
            .pending_after_candidate_loss_once
            .as_ref()
            .is_some_and(|flag| flag.swap(false, Ordering::SeqCst))
        {
            return Ok(CycleDispatchResult::unfinished());
        }
        let result = self.worker.run_cycle(envelope.clone())?;
        if result.terminal_candidate
            && self
                .fail_after_candidate_once
                .as_ref()
                .is_some_and(|flag| flag.swap(false, Ordering::SeqCst))
        {
            if let Some(flag) = &self.pending_after_candidate_loss_once {
                flag.store(true, Ordering::SeqCst);
            }
            return Err(
                "retryable distributed v2 delivery conflict: candidate acknowledgement lost"
                    .to_string(),
            );
        }
        Ok(result)
    }
}

fn checkpoint_config(store: InMemoryCheckpointStoreV2, key: &str) -> CheckpointConfig {
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
    let store = InMemoryCheckpointStoreV2::new();
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
        .load_checkpoint_v2("typed-checkpoint")
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
async fn runner_distributed_v2_worker_returns_candidate_and_runner_finalizes_once() {
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
    let store = InMemoryCheckpointStoreV2::new();
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
    let backend = DistributedBackend::distributed_with_dispatcher(
        recipe,
        Arc::new(InMemoryStateStore::new()),
        dispatcher,
    );
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
    checkpoint.key = Some("runner-distributed-v2".to_string());
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
        .load_checkpoint_v2("runner-distributed-v2")
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
    let store = InMemoryCheckpointStoreV2::new();
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
    let backend = DistributedBackend::distributed_with_dispatcher(
        recipe,
        Arc::new(InMemoryStateStore::new()),
        dispatcher,
    )
    .with_lease_duration(Duration::from_millis(75))
    .with_dispatch_timeout(Duration::from_secs(2));
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
        .load_checkpoint_v2("candidate-ack-loss")
        .expect("load checkpoint")
        .expect("terminal checkpoint");
    assert_eq!(terminal.resume_attempt, 2);
    assert_eq!(terminal.status, CheckpointStatus::Completed);
    assert!(terminal.terminal_acknowledged);
}

#[tokio::test]
async fn distributed_dispatch_failure_preserves_root_error_and_external_claim() {
    let store = InMemoryCheckpointStoreV2::new();
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
    let backend = DistributedBackend::distributed_with_dispatcher(
        recipe,
        Arc::new(InMemoryStateStore::new()),
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
        .load_checkpoint_v2("dispatch-failure")
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
async fn distributed_v2_commits_nonterminal_cycle_before_max_cycles_candidate() {
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
    let store = InMemoryCheckpointStoreV2::new();
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
    let backend = DistributedBackend::distributed_with_dispatcher(
        recipe,
        Arc::new(InMemoryStateStore::new()),
        dispatcher,
    );
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
        .load_checkpoint_v2("distributed-multicycle")
        .unwrap()
        .unwrap();
    assert_eq!(terminal.status, CheckpointStatus::MaxCycles);
    assert_eq!(terminal.cycle_index, 2);
    assert_eq!(terminal.cycles.len(), 2);
    assert!(terminal.terminal_acknowledged);
}

#[tokio::test]
async fn runner_recovery_stops_before_ambiguous_non_idempotent_tool() {
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
        .idempotency(ToolIdempotency::Unknown)
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
    let store = InMemoryCheckpointStoreV2::new();
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
        .load_checkpoint_v2("ambiguous-runner")
        .expect("load checkpoint")
        .expect("checkpoint");
    let arguments = serde_json::Map::from_iter([("value".to_string(), json!("42"))]);
    let idempotency_key = "idem_ambiguous_runner";
    let request_digest = tool_request_digest(
        "call-unsafe",
        "unsafe_write",
        &Value::Object(arguments.clone()),
        idempotency_key,
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
        idempotency_key,
        ToolIdempotency::Unknown,
    );
    started
        .transition_to(OperationState::Started)
        .expect("started operation");
    crashed.tool_journal = vec![started];
    crashed.lease_expires_at_ms = Some(1);
    store
        .save_checkpoint_v2(crashed)
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
    assert_eq!(resumed.status(), AgentStatus::ReconciliationRequired);
    assert!(resumed.completion_reason().is_none());
    assert!(resumed.resume_observation().is_some());
    assert!(resumed.new_items().is_empty());
    assert_eq!(model_calls.load(Ordering::SeqCst), 0);
    assert_eq!(tool_effects.load(Ordering::SeqCst), 0);

    let retained = store
        .load_checkpoint_v2("ambiguous-runner")
        .expect("load retained checkpoint")
        .expect("retained checkpoint");
    assert_eq!(retained.status, CheckpointStatus::ReconciliationRequired);
    assert_eq!(retained.tool_journal[0].state, OperationState::Ambiguous);
    assert!(retained.claim_token.is_none());
    assert!(retained.terminal_result.is_none());
}

fn run_config(
    store: InMemoryCheckpointStoreV2,
    session: MemorySession,
    crash_once: Arc<AtomicBool>,
) -> RunConfig {
    RunConfig::builder()
        .max_cycles(2)
        .no_tool_policy(NoToolPolicy::Finish)
        .session(session)
        .checkpoint_config(checkpoint_config(store, "runner-checkpoint"))
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
    let model_calls = Arc::new(AtomicUsize::new(0));
    let first_calls = model_calls.clone();
    let second_calls = model_calls.clone();
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "checkpoint-model",
        vec![
            ScriptStep::callback(move |_request| {
                first_calls.fetch_add(1, Ordering::SeqCst);
                Ok(LLMResponse::with_tool_calls(
                    "write once",
                    vec![ToolCall::new("call-write", "write_record", BTreeMap::new())],
                ))
            }),
            ScriptStep::callback(move |_request| {
                second_calls.fetch_add(1, Ordering::SeqCst);
                Ok(LLMResponse::new("done"))
            }),
        ],
    );
    let observed_keys = Arc::new(Mutex::new(Vec::<String>::new()));
    let keys_for_tool = observed_keys.clone();
    let tool = FunctionTool::builder("write_record")
        .description("Record one idempotent side effect.")
        .json_schema(json!({
            "type": "object",
            "properties": {},
            "required": []
        }))
        .idempotency(ToolIdempotency::Supported)
        .handler(move |context, _arguments: Value| {
            let keys = keys_for_tool.clone();
            async move {
                keys.lock()
                    .expect("idempotency keys")
                    .push(context.idempotency_key.expect("stable idempotency key"));
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
    let agent = Agent::builder("checkpoint-agent")
        .instructions("Write the record, then return the final answer.")
        .model(ModelRef::named("checkpoint-model"))
        .tool(tool)
        .build()
        .expect("agent");
    let store = InMemoryCheckpointStoreV2::new();
    let session = MemorySession::new("runner-checkpoint-session");
    let crash_once = Arc::new(AtomicBool::new(true));

    let first = runner
        .run_with_config(
            &agent,
            "process item 42",
            run_config(store.clone(), session.clone(), crash_once.clone()),
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
    let keys = observed_keys.lock().expect("idempotency keys").clone();
    assert_eq!(keys.len(), 1);
    assert!(keys[0].starts_with("idem_"));

    let mut crashed = store
        .load_checkpoint_v2("runner-checkpoint")
        .expect("load crashed checkpoint")
        .expect("crashed checkpoint");
    assert_eq!(crashed.cycle_index, 1);
    assert_eq!(crashed.cycles.len(), 1);
    assert_eq!(crashed.resume_attempt, 1);
    assert!(crashed.claim_token.is_some());
    let original_run_id = crashed.root_run_id.clone();
    let original_trace_id = crashed.trace_id.clone();
    crashed.lease_expires_at_ms = Some(1);
    store
        .save_checkpoint_v2(crashed)
        .expect("expire crashed claim");

    let resumed = runner
        .run_with_config(
            &agent,
            "process item 42",
            run_config(store.clone(), session.clone(), crash_once.clone()),
        )
        .await
        .expect("resume");
    assert_eq!(resumed.status(), AgentStatus::Completed);
    assert_eq!(resumed.final_output(), Some("done"));
    assert_eq!(resumed.run_id(), original_run_id);
    assert_eq!(resumed.trace_id(), original_trace_id);
    assert_eq!(resumed.result().cycles.len(), 2);
    assert_eq!(model_calls.load(Ordering::SeqCst), 2);
    assert_eq!(observed_keys.lock().expect("idempotency keys").len(), 1);

    let terminal = store
        .load_checkpoint_v2("runner-checkpoint")
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
            run_config(store.clone(), session.clone(), crash_once),
        )
        .await
        .expect("terminal replay");
    assert_eq!(replay.status(), AgentStatus::Completed);
    assert_eq!(replay.final_output(), Some("done"));
    assert_eq!(replay.run_id(), original_run_id);
    assert_eq!(replay.trace_id(), original_trace_id);
    assert_eq!(model_calls.load(Ordering::SeqCst), 2);
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
