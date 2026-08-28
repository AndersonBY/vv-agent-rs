use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use vv_agent::runtime::backends::distributed::CycleEnqueuer;
use vv_agent::{
    Agent, AgentStatus, AgentTask, CapabilityRef, CheckpointConfig, CheckpointStore, ClaimMode,
    ContextError, ContextFragment, ContextProvider, ContextRequest, DistributedAdvanceDecision,
    DistributedBackend, DistributedCapabilities, DistributedCapabilityRegistry,
    DistributedCycleWorker, DistributedDeliveryOutcome, DistributedRunEnvelope, ExecutionMode,
    InMemoryCheckpointStore, LLMResponse, MemorySession, ModelRef, NoToolPolicy, PromptBundle,
    ResumePolicy, RunConfig, Runner, RuntimeRecipe, ScriptedLlmClient, ScriptedModelProvider,
    Session,
};

#[derive(Default)]
struct RecordingEnqueuer {
    envelopes: Mutex<Vec<DistributedRunEnvelope>>,
}

#[derive(Clone, Default)]
struct CountingContextProvider {
    calls: Arc<AtomicUsize>,
}

impl ContextProvider for CountingContextProvider {
    fn fragments(
        &self,
        _request: &ContextRequest<'_>,
    ) -> Result<Vec<ContextFragment>, ContextError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Vec::new())
    }
}

impl RecordingEnqueuer {
    fn take_one(&self) -> DistributedRunEnvelope {
        let mut envelopes = self.envelopes.lock().expect("envelopes");
        assert_eq!(envelopes.len(), 1);
        envelopes.remove(0)
    }

    fn len(&self) -> usize {
        self.envelopes.lock().expect("envelopes").len()
    }
}

impl CycleEnqueuer for RecordingEnqueuer {
    fn enqueue_envelope(
        &self,
        envelope: &DistributedRunEnvelope,
        _not_before_unix_ms: Option<u64>,
    ) -> Result<(), String> {
        self.envelopes
            .lock()
            .map_err(|_| "envelopes lock poisoned".to_string())?
            .push(envelope.clone());
        Ok(())
    }
}

struct DistributedFixture {
    runner: Runner,
    agent: Agent,
    config: RunConfig,
    backend: DistributedBackend,
    worker: DistributedCycleWorker,
    enqueuer: Arc<RecordingEnqueuer>,
    store: InMemoryCheckpointStore,
    session: MemorySession,
}

fn distributed_fixture(key: &str, max_cycles: u32) -> DistributedFixture {
    let store = InMemoryCheckpointStore::new();
    distributed_fixture_with_stores(key, max_cycles, store.clone(), store)
}

fn distributed_fixture_with_stores(
    key: &str,
    max_cycles: u32,
    controller_store: InMemoryCheckpointStore,
    registry_store: InMemoryCheckpointStore,
) -> DistributedFixture {
    let checkpoint_ref = CapabilityRef::new("checkpoint.runner-driver", "1").unwrap();
    let llm_ref = CapabilityRef::new("llm.runner-driver", "1").unwrap();
    let registry = DistributedCapabilityRegistry::new();
    registry.register_checkpoint_store(checkpoint_ref.clone(), Arc::new(registry_store));
    registry.register_llm_client(
        llm_ref.clone(),
        Arc::new(ScriptedLlmClient::new(vec![LLMResponse::new("done")])),
    );
    let mut recipe = RuntimeRecipe::new("settings.json", "scripted", "driver-model", ".");
    recipe.capabilities = DistributedCapabilities {
        checkpoint_store_ref: Some(checkpoint_ref.clone()),
        llm_client_ref: Some(llm_ref),
        ..DistributedCapabilities::default()
    };
    let enqueuer = Arc::new(RecordingEnqueuer::default());
    let backend = DistributedBackend::nonblocking(recipe, registry.clone(), enqueuer.clone());
    let worker = DistributedCycleWorker::new(registry);

    let mut checkpoint = CheckpointConfig::with_store(controller_store.clone());
    checkpoint.key = Some(key.to_string());
    checkpoint.resume_policy = ResumePolicy::ResumeIfPresent;
    checkpoint
        .capability_refs
        .insert("checkpoint_store".to_string(), checkpoint_ref);
    checkpoint.capability_refs.insert(
        "session".to_string(),
        CapabilityRef::new("session.runner-driver", "1").unwrap(),
    );
    let session = MemorySession::new(format!("{key}-session"));
    let config = RunConfig::builder()
        .max_cycles(max_cycles)
        .no_tool_policy(NoToolPolicy::Finish)
        .session(session.clone())
        .checkpoint_config(checkpoint)
        .execution_mode(ExecutionMode::Distributed(backend.clone()))
        .build();
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "driver-model",
            Vec::new(),
        ))
        .workspace(".")
        .build()
        .expect("runner");
    let agent = Agent::builder("distributed-runner-agent")
        .instructions("Return the answer.")
        .model(ModelRef::named("driver-model"))
        .build()
        .expect("agent");
    DistributedFixture {
        runner,
        agent,
        config,
        backend,
        worker,
        enqueuer,
        store: controller_store,
        session,
    }
}

#[tokio::test]
async fn runner_starts_passively_and_finalizes_claimed_candidate_once() {
    let fixture = distributed_fixture("runner-distributed-candidate", 2);
    let handle = fixture
        .runner
        .start_distributed(&fixture.agent, "answer", fixture.config.clone())
        .await
        .expect("passive start");
    let envelope = fixture.enqueuer.take_one();
    assert_eq!(envelope.cycle_index, 1);
    assert_eq!(envelope.claim_mode, ClaimMode::Continue);
    assert_eq!(handle.checkpoint_key, "runner-distributed-candidate");

    let worker_result = fixture
        .worker
        .run_cycle(envelope.clone())
        .expect("single cycle worker");
    let decision = fixture
        .backend
        .advance(&envelope, DistributedDeliveryOutcome::worker(worker_result))
        .expect("advance candidate");
    assert!(matches!(
        decision,
        DistributedAdvanceDecision::FinalizeRequired { .. }
    ));

    let finalized = fixture
        .runner
        .finalize_distributed(
            &fixture.agent,
            "answer",
            decision.clone(),
            fixture.config.clone(),
        )
        .await
        .expect("bounded finalizer");
    assert_eq!(finalized.status(), AgentStatus::Completed);
    assert_eq!(finalized.final_output(), Some("done"));
    let persisted = fixture
        .store
        .load_checkpoint("runner-distributed-candidate")
        .unwrap()
        .unwrap();
    assert!(persisted.terminal_result.is_some());
    assert!(persisted.terminal_acknowledged);
    assert!(persisted.claim_token.is_none());
    let session_count = fixture.session.get_items(None).await.unwrap().len();
    assert!(session_count > 0);

    let replayed = fixture
        .runner
        .finalize_distributed(&fixture.agent, "answer", decision, fixture.config.clone())
        .await
        .expect("duplicate finalizer replay");
    assert_eq!(replayed.result(), finalized.result());
    assert_eq!(
        fixture.session.get_items(None).await.unwrap().len(),
        session_count
    );
}

#[tokio::test]
async fn runner_starts_distributed_from_compiled_task_without_rebuilding_it() {
    let mut fixture = distributed_fixture("runner-distributed-compiled", 2);
    let context_provider = CountingContextProvider::default();
    fixture
        .config
        .context_providers
        .push(Arc::new(context_provider.clone()));
    fixture
        .config
        .checkpoint_config
        .as_mut()
        .expect("checkpoint config")
        .capability_refs
        .insert(
            "context_provider:0".to_string(),
            CapabilityRef::new("context.runner-driver", "1").expect("context provider ref"),
        );
    let mut task = AgentTask::new(
        "runner-distributed-compiled",
        "driver-model",
        PromptBundle::from_instruction_text("compiled instructions").expect("prompt bundle"),
        "compiled input",
    );
    task.max_cycles = 2;
    task.allow_interruption = false;
    task.use_workspace = false;
    task.agent_type = Some("computer".to_string());
    task.extra_tool_names = vec!["compiled_tool".to_string()];
    task.initial_messages = vec![vv_agent::Message::user("compiled history")];
    task.metadata.insert(
        "compiled_marker".to_string(),
        serde_json::Value::String("preserved".to_string()),
    );

    let expected_prompt_bundle = task.prompt_bundle.clone();
    let expected_initial_messages = task.initial_messages.clone();
    let handle = fixture
        .runner
        .start_distributed_compiled(&fixture.agent, task, fixture.config.clone())
        .await
        .expect("compiled passive start");
    let envelope = fixture.enqueuer.take_one();

    assert_eq!(handle.checkpoint_key, "runner-distributed-compiled");
    assert_eq!(envelope.task.user_prompt, "compiled input");
    assert_eq!(envelope.task.prompt_bundle, expected_prompt_bundle);
    assert_eq!(envelope.task.max_cycles, 2);
    assert!(!envelope.task.allow_interruption);
    assert!(!envelope.task.use_workspace);
    assert_eq!(envelope.task.agent_type.as_deref(), Some("computer"));
    assert_eq!(envelope.task.extra_tool_names, vec!["compiled_tool"]);
    assert_eq!(envelope.task.initial_messages, expected_initial_messages);
    assert_eq!(
        envelope.task.metadata.get("compiled_marker"),
        Some(&serde_json::Value::String("preserved".to_string()))
    );
    assert_eq!(context_provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn distributed_start_rejects_controller_registry_store_conflict_before_enqueue() {
    let key = "runner-distributed-store-conflict";
    let first = distributed_fixture(key, 2);
    first
        .runner
        .start_distributed(&first.agent, "answer", first.config.clone())
        .await
        .expect("seed passive start");
    let first_envelope = first.enqueuer.take_one();
    let checkpoint = first
        .store
        .load_checkpoint(key)
        .expect("load controller checkpoint")
        .expect("controller checkpoint");

    let registry_store = InMemoryCheckpointStore::new();
    registry_store
        .save_checkpoint(checkpoint.clone())
        .expect("seed registry checkpoint");
    let mismatch =
        distributed_fixture_with_stores(key, 2, first.store.clone(), registry_store.clone());
    let error = mismatch
        .runner
        .start_distributed(&mismatch.agent, "answer", mismatch.config.clone())
        .await
        .expect_err("different controller and registry stores must be rejected");
    assert!(error.contains("checkpoint_store_conflict"), "{error}");
    assert_eq!(mismatch.enqueuer.len(), 0);
    assert_eq!(
        first
            .store
            .load_checkpoint(key)
            .expect("reload controller checkpoint")
            .expect("controller checkpoint remains")
            .revision,
        checkpoint.revision
    );
    assert_eq!(
        registry_store
            .load_checkpoint(key)
            .expect("reload registry checkpoint")
            .expect("registry checkpoint remains")
            .revision,
        checkpoint.revision
    );
    assert_eq!(first_envelope.checkpoint_config.key, key);
}

#[tokio::test]
async fn runner_finalizes_unclaimed_max_cycles_decision() {
    let fixture = distributed_fixture("runner-distributed-max", 1);
    let _handle = fixture
        .runner
        .start_distributed(&fixture.agent, "answer", fixture.config.clone())
        .await
        .expect("passive start");
    let envelope = fixture.enqueuer.take_one();
    let worker_result = fixture
        .worker
        .run_cycle(envelope.clone())
        .expect("single cycle worker");
    let committed = match worker_result {
        vv_agent::CycleDispatchResult::TerminalCandidate {
            checkpoint_revision,
            ..
        } => {
            let checkpoint = fixture
                .store
                .load_checkpoint("runner-distributed-max")
                .unwrap()
                .unwrap();
            let claim_token = checkpoint.claim_token.clone().expect("candidate claim");
            let mut committed_checkpoint = checkpoint;
            committed_checkpoint.cycle_index = 1;
            assert!(fixture
                .store
                .commit_checkpoint(committed_checkpoint, &claim_token, checkpoint_revision)
                .unwrap());
            vv_agent::CycleDispatchResult::committed(1, checkpoint_revision + 1).unwrap()
        }
        other => panic!("expected terminal candidate, got {other:?}"),
    };
    let decision = fixture
        .backend
        .advance(&envelope, DistributedDeliveryOutcome::worker(committed))
        .expect("max cycles decision");
    assert!(matches!(
        decision,
        DistributedAdvanceDecision::FinalizeRequired { ref result, .. }
            if result.status == AgentStatus::MaxCycles
    ));

    let finalized = fixture
        .runner
        .finalize_distributed(&fixture.agent, "answer", decision, fixture.config)
        .await
        .expect("unclaimed max cycles finalizer");
    assert_eq!(finalized.status(), AgentStatus::MaxCycles);
    let persisted = fixture
        .store
        .load_checkpoint("runner-distributed-max")
        .unwrap()
        .unwrap();
    assert_eq!(persisted.status, vv_agent::CheckpointStatus::MaxCycles);
    assert!(persisted.terminal_acknowledged);
    assert!(persisted.claim_token.is_none());
}
