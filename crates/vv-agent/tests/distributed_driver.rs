use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use vv_agent::runtime::backends::distributed::{
    CapabilityRef, CycleEnqueuer, DistributedAdvanceDecision, DistributedBackend,
    DistributedCapabilities, DistributedCapabilityRegistry, DistributedCheckpointConfig,
    DistributedCycleWorker, DistributedDeliveryOutcome, DistributedRunEnvelope,
    DistributedWaitReason, DEFAULT_CYCLE_NAME,
};
use vv_agent::runtime::checkpoint_codec::checkpoint_from_value;
use vv_agent::types::AgentTask;
use vv_agent::{
    AgentResult, AmbiguousModelPolicy, AmbiguousToolPolicy, CheckpointStatus, CheckpointStore,
    ClaimMode, CycleDispatchResult, DeferredBatchEntry, DeferredToolHandle,
    InMemoryCheckpointStore, OperationJournalEntry, OperationState, PromptBundle, ResumePolicy,
    RuntimeRecipe, ToolCallOutcome, ToolIdempotency,
};

const CODEC_FIXTURE: &str = include_str!("fixtures/parity/checkpoint_codec.json");

#[derive(Default)]
struct RecordingEnqueuer {
    deliveries: Mutex<Vec<(DistributedRunEnvelope, Option<u64>)>>,
}

impl RecordingEnqueuer {
    fn deliveries(&self) -> Vec<(DistributedRunEnvelope, Option<u64>)> {
        self.deliveries.lock().expect("deliveries").clone()
    }
}

impl CycleEnqueuer for RecordingEnqueuer {
    fn enqueue_envelope(
        &self,
        envelope: &DistributedRunEnvelope,
        not_before_unix_ms: Option<u64>,
    ) -> Result<(), String> {
        self.deliveries
            .lock()
            .map_err(|_| "deliveries lock poisoned".to_string())?
            .push((envelope.clone(), not_before_unix_ms));
        Ok(())
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis()
        .try_into()
        .expect("milliseconds")
}

fn minimal_checkpoint(key: &str) -> vv_agent::Checkpoint {
    fixture_checkpoint("minimal_running", key)
}

fn admitted_deferred_checkpoint(key: &str) -> (vv_agent::Checkpoint, Arc<InMemoryCheckpointStore>) {
    let digest = "a".repeat(64);
    let operation_id = "op_tool_cycle_1_call_deferred";
    let tool_call_id = "call_deferred";
    let mut checkpoint = minimal_checkpoint(key);
    let mut journal = OperationJournalEntry::tool(
        operation_id,
        1,
        1,
        digest.clone(),
        tool_call_id,
        "remote_write",
        BTreeMap::new().into_iter().collect(),
        None,
        ToolIdempotency::Unsupported,
    );
    journal.state = OperationState::Started;
    checkpoint.tool_journal = vec![journal];
    checkpoint.validate().expect("started deferred checkpoint");
    let store = InMemoryCheckpointStore::new();
    store
        .create_checkpoint(checkpoint.clone())
        .expect("create started checkpoint");
    let claimed = store
        .claim_checkpoint(key, 1, "claim-deferred", 10_000, 1, ClaimMode::Continue)
        .expect("claim deferred checkpoint")
        .expect("claimed deferred checkpoint");
    let handle =
        DeferredToolHandle::new(key, operation_id, 1, digest.clone()).expect("deferred handle");
    let admission = store
        .admit_deferred_batch(
            key,
            claimed.revision,
            "claim-deferred",
            1,
            &[DeferredBatchEntry {
                operation_id: operation_id.to_string(),
                cycle_index: 1,
                attempt: 1,
                request_digest: digest,
                tool_call_id: tool_call_id.to_string(),
                tool_name: "remote_write".to_string(),
                idempotency_key: None,
                idempotency_support: ToolIdempotency::Unsupported,
                outcome: ToolCallOutcome::deferred(handle),
            }],
        )
        .expect("deferred admission");
    assert_eq!(admission.checkpoint.status, CheckpointStatus::Deferred);
    assert!(admission.checkpoint.claim_token.is_none());
    (admission.checkpoint, Arc::new(store))
}

fn fixture_checkpoint(name: &str, key: &str) -> vv_agent::Checkpoint {
    let fixture: Value = serde_json::from_str(CODEC_FIXTURE).expect("checkpoint fixture");
    let mut payload = fixture["valid_cases"]
        .as_array()
        .expect("valid cases")
        .iter()
        .find(|case| case["name"] == name)
        .unwrap_or_else(|| panic!("missing checkpoint fixture {name}"))["payload"]
        .clone();
    payload["checkpoint_key"] = json!(key);
    payload["task_id"] = json!(format!("{key}-task"));
    payload["root_run_id"] = json!(format!("{key}-run"));
    payload["trace_id"] = json!(format!("{key}-trace"));
    checkpoint_from_value(&payload, 262_144).expect("valid checkpoint")
}

fn task(checkpoint: &vv_agent::Checkpoint, max_cycles: u32) -> AgentTask {
    let mut task = AgentTask::new(
        checkpoint.task_id.clone(),
        "test-model",
        PromptBundle::from_instruction_text("You are a careful assistant.").expect("prompt bundle"),
        "Summarize the status.",
    );
    task.max_cycles = max_cycles;
    task.memory_compact_threshold = checkpoint.run_definition["runtime_controls"]
        ["memory_compact_threshold"]
        .as_u64()
        .expect("memory threshold");
    task.use_workspace = false;
    task.exclude_tools = vec!["task_finish".to_string(), "ask_user".to_string()];
    task.metadata.insert(
        "session_memory_enabled".to_string(),
        checkpoint.run_definition["runtime_controls"]["session_memory_enabled"].clone(),
    );
    task
}

fn checkpoint_ref() -> CapabilityRef {
    CapabilityRef::new("checkpoint.driver", "1").expect("checkpoint ref")
}

fn recipe() -> RuntimeRecipe {
    let mut recipe = RuntimeRecipe::new("settings.json", "test", "test-model", ".");
    recipe.capabilities = DistributedCapabilities {
        checkpoint_store_ref: Some(checkpoint_ref()),
        ..DistributedCapabilities::default()
    };
    recipe
}

fn checkpoint_config(checkpoint: &vv_agent::Checkpoint) -> DistributedCheckpointConfig {
    DistributedCheckpointConfig {
        key: checkpoint.checkpoint_key.clone(),
        resume_policy: ResumePolicy::RequireExisting,
        ambiguous_model_policy: AmbiguousModelPolicy::RequireReconciliation,
        ambiguous_tool_policy: AmbiguousToolPolicy::RequireReconciliation,
        required_extension_namespaces: Vec::new(),
        max_extension_state_bytes: 262_144,
        credential_slots: Vec::new(),
    }
}

fn envelope(
    checkpoint: &vv_agent::Checkpoint,
    task: AgentTask,
    recipe: RuntimeRecipe,
    cycle_index: u32,
) -> DistributedRunEnvelope {
    DistributedRunEnvelope::for_cycle(
        task,
        recipe,
        cycle_index,
        DEFAULT_CYCLE_NAME,
        Some(checkpoint.root_run_id.clone()),
        Some(now_unix_ms() + 60_000),
        10_000,
        None,
        checkpoint.root_run_id.clone(),
        checkpoint.trace_id.clone(),
        checkpoint.run_definition_digest.clone(),
        ClaimMode::Continue,
        checkpoint.resume_attempt,
        checkpoint_config(checkpoint),
    )
    .expect("envelope")
}

fn build_backend(
    checkpoint: vv_agent::Checkpoint,
    recipe: RuntimeRecipe,
) -> (
    DistributedBackend,
    Arc<InMemoryCheckpointStore>,
    Arc<RecordingEnqueuer>,
) {
    let store = Arc::new(InMemoryCheckpointStore::new());
    store
        .create_checkpoint(checkpoint)
        .expect("create checkpoint");
    let registry = DistributedCapabilityRegistry::new();
    registry.register_checkpoint_store(checkpoint_ref(), store.clone());
    let enqueuer = Arc::new(RecordingEnqueuer::default());
    let backend = DistributedBackend::nonblocking(recipe, registry, enqueuer.clone())
        .with_dispatch_timeout(Duration::from_secs(60));
    (backend, store, enqueuer)
}

#[test]
fn start_enqueues_only_cycle_one_and_returns_passive_handle() {
    let checkpoint = minimal_checkpoint("driver-start");
    let config = checkpoint_config(&checkpoint);
    let task = task(&checkpoint, 10);
    let (backend, _store, enqueuer) = build_backend(checkpoint.clone(), recipe());

    let handle = backend.start(task, config, None).expect("start");

    assert_eq!(handle.checkpoint_key, checkpoint.checkpoint_key);
    assert_eq!(handle.run_id, checkpoint.root_run_id);
    assert_eq!(handle.trace_id, checkpoint.trace_id);
    let deliveries = enqueuer.deliveries();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].0.cycle_index, 1);
    assert_eq!(deliveries[0].0.claim_mode, ClaimMode::Continue);
    assert_eq!(deliveries[0].1, None);
}

#[test]
fn deferred_producer_and_redelivery_remain_pending_without_claim_or_enqueue() {
    let key = "driver-deferred-redelivery";
    let (checkpoint, store) = admitted_deferred_checkpoint(key);
    let registry = DistributedCapabilityRegistry::new();
    registry.register_checkpoint_store(checkpoint_ref(), store.clone());
    let worker = DistributedCycleWorker::new(registry.clone());
    let enqueuer = Arc::new(RecordingEnqueuer::default());
    let backend = DistributedBackend::nonblocking(recipe(), registry, enqueuer.clone());
    let envelope = envelope(&checkpoint, task(&checkpoint, 10), recipe(), 1);
    let before = store
        .load_checkpoint(key)
        .expect("load deferred checkpoint")
        .expect("deferred checkpoint");

    let first = worker
        .run_cycle(envelope.clone())
        .expect("first deferred delivery");
    let repeated = worker
        .run_cycle(envelope.clone())
        .expect("repeated deferred delivery");
    assert_eq!(first, CycleDispatchResult::pending());
    assert_eq!(repeated, CycleDispatchResult::pending());

    let decision = backend
        .advance(&envelope, DistributedDeliveryOutcome::worker(first.clone()))
        .expect("deferred driver advance");
    assert!(matches!(
        decision,
        DistributedAdvanceDecision::Wait {
            reason: DistributedWaitReason::DeferredPending,
            ..
        }
    ));
    assert!(enqueuer.deliveries().is_empty());
    let after = store
        .load_checkpoint(key)
        .expect("reload deferred checkpoint")
        .expect("deferred checkpoint remains");
    assert_eq!(after.status, CheckpointStatus::Deferred);
    assert_eq!(after.claim_token, None);
    assert_eq!(after.revision, before.revision);
    assert_eq!(after.event_outbox, before.event_outbox);
}

#[test]
fn committed_checkpoint_dispatches_the_next_cycle_once() {
    let mut checkpoint = minimal_checkpoint("driver-next");
    checkpoint.cycle_index = 1;
    checkpoint.revision = 3;
    let previous = envelope(&checkpoint, task(&checkpoint, 10), recipe(), 1);
    let (backend, _store, enqueuer) = build_backend(checkpoint, recipe());

    let decision = backend
        .advance(
            &previous,
            DistributedDeliveryOutcome::worker(
                CycleDispatchResult::committed(1, 3).expect("committed"),
            ),
        )
        .expect("advance");

    assert!(matches!(
        decision,
        DistributedAdvanceDecision::Dispatch { ref envelope, .. }
            if envelope.cycle_index == 2 && envelope.claim_mode == ClaimMode::Continue
    ));
    assert_eq!(enqueuer.deliveries().len(), 1);
}

#[test]
fn max_cycles_requires_framework_finalization_without_enqueue() {
    let mut checkpoint = minimal_checkpoint("driver-max");
    checkpoint.cycle_index = 1;
    checkpoint.revision = 2;
    let previous = envelope(&checkpoint, task(&checkpoint, 1), recipe(), 1);
    let (backend, _store, enqueuer) = build_backend(checkpoint, recipe());

    let decision = backend
        .advance(
            &previous,
            DistributedDeliveryOutcome::worker(
                CycleDispatchResult::committed(1, 2).expect("committed"),
            ),
        )
        .expect("advance");

    assert!(matches!(
        decision,
        DistributedAdvanceDecision::FinalizeRequired { ref result, .. }
            if result.status == vv_agent::AgentStatus::MaxCycles
    ));
    assert!(enqueuer.deliveries().is_empty());
}

#[test]
fn terminal_candidate_retains_claim_for_separate_finalizer() {
    let mut checkpoint = minimal_checkpoint("driver-candidate");
    checkpoint.claim_token = Some("claim-1".to_string());
    checkpoint.claimed_cycle = Some(1);
    checkpoint.lease_expires_at_ms = Some(now_unix_ms() + 60_000);
    checkpoint.revision = 1;
    let previous = envelope(&checkpoint, task(&checkpoint, 10), recipe(), 1);
    let candidate = AgentResult::completed(Vec::new(), Vec::new(), "done");
    let (backend, store, enqueuer) = build_backend(checkpoint, recipe());

    let decision = backend
        .advance(
            &previous,
            DistributedDeliveryOutcome::worker(
                CycleDispatchResult::terminal_candidate(candidate.clone(), 1).expect("candidate"),
            ),
        )
        .expect("advance");

    assert!(matches!(
        decision,
        DistributedAdvanceDecision::FinalizeRequired { result, .. } if result == candidate
    ));
    let persisted = store
        .load_checkpoint("driver-candidate")
        .expect("load")
        .expect("checkpoint");
    assert_eq!(persisted.claim_token.as_deref(), Some("claim-1"));
    assert!(persisted.terminal_result.is_none());
    assert!(enqueuer.deliveries().is_empty());
}

#[test]
fn terminal_candidate_is_revalidated_before_checkpoint_observation() {
    let mut checkpoint = minimal_checkpoint("driver-invalid-candidate");
    checkpoint.claim_token = Some("claim-invalid".to_string());
    checkpoint.claimed_cycle = Some(1);
    checkpoint.lease_expires_at_ms = Some(now_unix_ms() + 60_000);
    checkpoint.revision = 1;
    let previous = envelope(&checkpoint, task(&checkpoint, 10), recipe(), 1);
    let (backend, _store, enqueuer) = build_backend(checkpoint, recipe());
    let invalid = CycleDispatchResult::TerminalCandidate {
        checkpoint_revision: 1,
        result: AgentResult {
            status: vv_agent::AgentStatus::Running,
            ..AgentResult::default()
        },
    };

    let error = backend
        .advance(&previous, DistributedDeliveryOutcome::worker(invalid))
        .expect_err("invalid candidate");

    assert!(error.contains("complete current AgentResult"));
    assert!(enqueuer.deliveries().is_empty());
}

#[test]
fn durable_terminal_is_replayed_without_enqueue() {
    let mut checkpoint = minimal_checkpoint("driver-replay");
    let result = AgentResult::completed(Vec::new(), Vec::new(), "done");
    checkpoint.status = CheckpointStatus::Completed;
    checkpoint.terminal_result = Some(result.to_dict());
    checkpoint.revision = 4;
    let previous = envelope(&checkpoint, task(&checkpoint, 10), recipe(), 1);
    let (backend, _store, enqueuer) = build_backend(checkpoint, recipe());

    let decision = backend
        .advance(
            &previous,
            DistributedDeliveryOutcome::worker(
                CycleDispatchResult::terminal_replay(result.clone(), 4).expect("replay"),
            ),
        )
        .expect("advance");

    assert!(matches!(
        decision,
        DistributedAdvanceDecision::TerminalReplay { result: replay, .. } if replay == result
    ));
    assert!(enqueuer.deliveries().is_empty());
}

#[test]
fn transport_failure_with_live_claim_schedules_recovery_at_lease_expiry() {
    let mut checkpoint = minimal_checkpoint("driver-live-claim");
    let lease_expires_at_ms = now_unix_ms() + 60_000;
    checkpoint.claim_token = Some("claim-live".to_string());
    checkpoint.claimed_cycle = Some(1);
    checkpoint.lease_expires_at_ms = Some(lease_expires_at_ms);
    checkpoint.revision = 1;
    let previous = envelope(&checkpoint, task(&checkpoint, 10), recipe(), 1);
    let (backend, _store, enqueuer) = build_backend(checkpoint, recipe());

    let decision = backend
        .advance(
            &previous,
            DistributedDeliveryOutcome::transport_failure("lost callback")
                .expect("transport failure"),
        )
        .expect("advance");

    assert!(matches!(
        decision,
        DistributedAdvanceDecision::RetryAt {
            ref envelope,
            not_before_unix_ms,
            ..
        } if envelope.cycle_index == 1
            && envelope.claim_mode == ClaimMode::Recovery
            && not_before_unix_ms == lease_expires_at_ms
    ));
    let deliveries = enqueuer.deliveries();
    assert_eq!(deliveries[0].1, Some(lease_expires_at_ms));
    assert!(deliveries[0]
        .0
        .deadline_unix_ms
        .is_some_and(|deadline| deadline > lease_expires_at_ms));
}

#[test]
fn expired_claim_dispatches_recovery_immediately() {
    let mut checkpoint = minimal_checkpoint("driver-expired-claim");
    checkpoint.claim_token = Some("claim-expired".to_string());
    checkpoint.claimed_cycle = Some(1);
    checkpoint.lease_expires_at_ms = Some(now_unix_ms().saturating_sub(1));
    checkpoint.revision = 1;
    let previous = envelope(&checkpoint, task(&checkpoint, 10), recipe(), 1);
    let (backend, _store, enqueuer) = build_backend(checkpoint, recipe());

    let decision = backend
        .advance(
            &previous,
            DistributedDeliveryOutcome::transport_failure("worker lost")
                .expect("transport failure"),
        )
        .expect("advance");

    assert!(matches!(
        decision,
        DistributedAdvanceDecision::Dispatch { ref envelope, .. }
            if envelope.cycle_index == 1 && envelope.claim_mode == ClaimMode::Recovery
    ));
    assert_eq!(enqueuer.deliveries()[0].1, None);
}

#[test]
fn reconciliation_and_superseded_deliveries_are_no_op_waits() {
    let reconciliation = fixture_checkpoint(
        "reconciliation_required_retains_ambiguous_journal",
        "driver-reconciliation",
    );
    let previous = envelope(&reconciliation, task(&reconciliation, 10), recipe(), 1);
    let (backend, _store, enqueuer) = build_backend(reconciliation, recipe());
    assert!(matches!(
        backend
            .advance(
                &previous,
                DistributedDeliveryOutcome::transport_failure("ambiguous")
                    .expect("transport failure"),
            )
            .expect("advance"),
        DistributedAdvanceDecision::Wait {
            reason: DistributedWaitReason::ReconciliationRequired,
            ..
        }
    ));
    assert!(enqueuer.deliveries().is_empty());

    let mut superseded = minimal_checkpoint("driver-superseded");
    superseded.cycle_index = 2;
    superseded.revision = 5;
    let previous = envelope(&superseded, task(&superseded, 10), recipe(), 1);
    let (backend, _store, enqueuer) = build_backend(superseded, recipe());
    assert!(matches!(
        backend
            .advance(
                &previous,
                DistributedDeliveryOutcome::worker(
                    CycleDispatchResult::committed(1, 3).expect("old committed callback"),
                ),
            )
            .expect("advance"),
        DistributedAdvanceDecision::Wait {
            reason: DistributedWaitReason::SupersededDelivery,
            ..
        }
    ));
    assert!(enqueuer.deliveries().is_empty());
}

#[test]
fn brokered_approval_is_rejected_before_first_enqueue() {
    let checkpoint = minimal_checkpoint("driver-approval");
    let mut recipe = recipe();
    recipe.capabilities.approval_provider_ref =
        Some(CapabilityRef::new("approval.provider", "1").expect("provider ref"));
    recipe.capabilities.approval_broker_ref =
        Some(CapabilityRef::new("approval.broker", "1").expect("broker ref"));
    let config = checkpoint_config(&checkpoint);
    let task = task(&checkpoint, 10);
    let (backend, _store, enqueuer) = build_backend(checkpoint, recipe);

    let error = backend
        .start(task, config, None)
        .expect_err("approval rejection");

    assert!(error.contains("do not support brokered approval waits"));
    assert!(enqueuer.deliveries().is_empty());
}
