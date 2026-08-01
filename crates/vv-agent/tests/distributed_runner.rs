use std::sync::{Arc, Mutex};

use vv_agent::runtime::backends::distributed::CycleEnqueuer;
use vv_agent::{
    Agent, AgentStatus, CapabilityRef, CheckpointConfig, CheckpointStore, ClaimMode,
    DistributedAdvanceDecision, DistributedBackend, DistributedCapabilities,
    DistributedCapabilityRegistry, DistributedCycleWorker, DistributedDeliveryOutcome,
    DistributedRunEnvelope, ExecutionMode, InMemoryCheckpointStore, LLMResponse, MemorySession,
    ModelRef, NoToolPolicy, ResumePolicy, RunConfig, Runner, RuntimeRecipe, ScriptedLlmClient,
    ScriptedModelProvider, Session,
};

#[derive(Default)]
struct RecordingEnqueuer {
    envelopes: Mutex<Vec<DistributedRunEnvelope>>,
}

impl RecordingEnqueuer {
    fn take_one(&self) -> DistributedRunEnvelope {
        let mut envelopes = self.envelopes.lock().expect("envelopes");
        assert_eq!(envelopes.len(), 1);
        envelopes.remove(0)
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
    let checkpoint_ref = CapabilityRef::new("checkpoint.runner-driver", "1").unwrap();
    let llm_ref = CapabilityRef::new("llm.runner-driver", "1").unwrap();
    let store = InMemoryCheckpointStore::new();
    let registry = DistributedCapabilityRegistry::new();
    registry.register_checkpoint_store(checkpoint_ref.clone(), Arc::new(store.clone()));
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

    let mut checkpoint = CheckpointConfig::with_store(store.clone());
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
        store,
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
