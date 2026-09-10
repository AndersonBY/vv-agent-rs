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
    ClaimMode, ControllerCommand, ControllerCommandVariant, ControllerHandle, CycleDispatchResult,
    DeferredBatchEntry, DeferredToolHandle, HostInteractionAdmissionContext,
    HostInteractionMessage, HostInteractionRequest, InMemoryCheckpointStore, OperationJournalEntry,
    OperationState, PromptBundle, ResumePolicy, RuntimeRecipe, ToolCallOutcome, ToolIdempotency,
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

fn initial_checkpoint(mut checkpoint: vv_agent::Checkpoint) -> vv_agent::Checkpoint {
    checkpoint.resume_attempt = 1;
    checkpoint.cycle_index = 0;
    checkpoint.status = CheckpointStatus::Running;
    checkpoint.cancel_requested = false;
    checkpoint.active_host_interaction = None;
    checkpoint.suspended_origin = None;
    checkpoint.cycles.clear();
    checkpoint.model_calls.clear();
    checkpoint.event_cursor = None;
    checkpoint.event_outbox.clear();
    checkpoint.model_call_journal.clear();
    checkpoint.tool_journal.clear();
    checkpoint.revision = 0;
    checkpoint.claim_token = None;
    checkpoint.claimed_cycle = None;
    checkpoint.lease_expires_at_ms = None;
    checkpoint.terminal_result = None;
    checkpoint.terminal_acknowledged = false;
    checkpoint
}

fn create_claimed_snapshot(
    store: &InMemoryCheckpointStore,
    mut snapshot: vv_agent::Checkpoint,
    claim_token: &str,
    lease_expires_at_ms: u64,
    now_ms: u64,
) -> vv_agent::Checkpoint {
    let key = snapshot.checkpoint_key.clone();
    assert!(store
        .create_checkpoint(initial_checkpoint(snapshot.clone()))
        .expect("create initial checkpoint"));
    let claimed = store
        .claim_checkpoint(
            &key,
            1,
            claim_token,
            lease_expires_at_ms,
            now_ms,
            ClaimMode::Continue,
        )
        .expect("claim checkpoint")
        .expect("claimed checkpoint");
    snapshot.status = CheckpointStatus::Running;
    snapshot.resume_attempt = claimed.resume_attempt;
    snapshot.cycle_index = claimed.cycle_index;
    snapshot.revision = claimed.revision;
    snapshot.claim_token = claimed.claim_token.clone();
    snapshot.claimed_cycle = claimed.claimed_cycle;
    snapshot.lease_expires_at_ms = claimed.lease_expires_at_ms;
    assert!(store
        .progress_checkpoint(snapshot, claim_token, claimed.revision)
        .expect("progress claimed checkpoint"));
    store
        .load_checkpoint(&key)
        .expect("load progressed checkpoint")
        .expect("progressed checkpoint")
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
    let claimed = create_claimed_snapshot(&store, checkpoint, "claim-deferred", 10_000, 1);
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
    task.no_tool_policy = serde_json::from_value(
        checkpoint.run_definition["runtime_controls"]["no_tool_policy"].clone(),
    )
    .expect("durable no-tool policy");
    task.memory_compact_threshold = checkpoint.run_definition["runtime_controls"]
        ["memory_compact_threshold"]
        .as_u64()
        .expect("memory threshold");
    task.use_workspace = false;
    task.exclude_tools = vec!["ask_user".to_string()];
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
    let initial = initial_checkpoint(checkpoint.clone());
    store
        .create_checkpoint(initial.clone())
        .expect("create initial checkpoint");
    if checkpoint != initial {
        store
            .save_checkpoint(checkpoint)
            .expect("seed checkpoint state");
    }
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
fn advance_dispatches_host_response_without_consuming_or_claiming_it() {
    let key = "driver-host-response";
    let store = Arc::new(InMemoryCheckpointStore::new());
    let checkpoint = minimal_checkpoint(key);
    store
        .create_checkpoint(checkpoint.clone())
        .expect("create checkpoint");
    let now_ms = now_unix_ms();
    let lease_expires_at_ms = now_ms + 60_000;
    let claimed = store
        .claim_checkpoint(
            key,
            1,
            "worker-host-response",
            lease_expires_at_ms,
            now_ms,
            ClaimMode::Continue,
        )
        .expect("claim checkpoint")
        .expect("claimed checkpoint");
    let request = HostInteractionRequest::new(
        "interaction-driver-response",
        1,
        "operation-driver-response",
        "tool-driver-response",
        "Choose.",
    )
    .expect("request");
    let admitted = store
        .produce_host_interaction(
            request.clone(),
            &HostInteractionAdmissionContext::new(
                key,
                claimed.revision,
                "worker-host-response",
                1,
                now_ms,
                lease_expires_at_ms,
            )
            .expect("admission context"),
        )
        .expect("host interaction admission");
    assert_unmatched_terminal_replay_rejected(&store, key);
    let command = ControllerCommand::new(
        "command-driver-response",
        ControllerHandle::new(key, &checkpoint.root_run_id, &checkpoint.trace_id).expect("handle"),
        1,
        admitted.checkpoint_revision,
        ControllerCommandVariant::HostInteractionResponse {
            interaction_id: request.interaction_id.clone(),
            logical_cycle: request.logical_cycle,
            operation_id: request.operation_id.clone(),
            tool_call_id: request.tool_call_id.clone(),
            request_digest: request.request_digest.clone(),
            response: HostInteractionMessage::user("Accepted.").expect("response"),
        },
    )
    .expect("controller command");
    let receipt = match store
        .resolve_controller_command(command)
        .expect("resolve host response")
    {
        vv_agent::ControllerCommandResolution::Applied { receipt, wake } => {
            assert_eq!(wake.action, "recovery_dispatch");
            receipt
        }
        other => panic!("unexpected resolution: {other:?}"),
    };
    let current = store
        .load_checkpoint(key)
        .expect("load admitted checkpoint")
        .expect("admitted checkpoint");
    let previous = envelope(&current, task(&current, 10), recipe(), 1);
    let registry = DistributedCapabilityRegistry::new();
    registry.register_checkpoint_store(checkpoint_ref(), store.clone());
    let enqueuer = Arc::new(RecordingEnqueuer::default());
    let backend = DistributedBackend::nonblocking(recipe(), registry, enqueuer.clone());

    let decision = backend
        .advance(
            &previous,
            DistributedDeliveryOutcome::worker(CycleDispatchResult::pending()),
        )
        .expect("advance host response");
    assert!(matches!(
        decision,
        DistributedAdvanceDecision::Dispatch { ref envelope, .. }
            if envelope.cycle_index == 1
                && envelope.claim_mode == ClaimMode::Recovery
                && envelope.resume_attempt == 1
    ));
    assert_eq!(enqueuer.deliveries().len(), 1);
    let recovered = store
        .load_checkpoint(key)
        .expect("load recovered checkpoint")
        .expect("recovered checkpoint");
    assert_eq!(recovered, current);
    assert_eq!(
        store
            .get_controller_command_receipt(&receipt.command_id)
            .expect("load delivered receipt")
            .expect("delivered receipt")
            .outbox_state,
        "pending"
    );
}

#[test]
fn advance_dispatches_suspended_host_resume_without_consuming_it() {
    let key = "driver-suspended-host-resume";
    let store = Arc::new(InMemoryCheckpointStore::new());
    let checkpoint = minimal_checkpoint(key);
    store
        .create_checkpoint(checkpoint.clone())
        .expect("create checkpoint");
    let now_ms = now_unix_ms();
    let claimed = store
        .claim_checkpoint(
            key,
            1,
            "worker-suspended-host",
            now_ms + 60_000,
            now_ms,
            ClaimMode::Continue,
        )
        .expect("claim checkpoint")
        .expect("claimed checkpoint");
    let request = HostInteractionRequest::new(
        "interaction-suspended-driver",
        1,
        "operation-suspended-driver",
        "tool-suspended-driver",
        "Choose.",
    )
    .expect("request");
    let admitted = store
        .produce_host_interaction(
            request.clone(),
            &HostInteractionAdmissionContext::new(
                key,
                claimed.revision,
                "worker-suspended-host",
                1,
                now_ms,
                claimed.lease_expires_at_ms.expect("claim lease"),
            )
            .expect("admission context"),
        )
        .expect("host interaction admission");
    let handle =
        ControllerHandle::new(key, &checkpoint.root_run_id, &checkpoint.trace_id).expect("handle");
    let suspend = ControllerCommand::new(
        "command-suspended-driver",
        handle.clone(),
        1,
        admitted.checkpoint_revision,
        ControllerCommandVariant::Suspend,
    )
    .expect("suspend command");
    let suspended_revision = match store.resolve_controller_command(suspend).expect("suspend") {
        vv_agent::ControllerCommandResolution::Applied { receipt, .. } => {
            receipt.resulting_revision
        }
        other => panic!("unexpected suspend resolution: {other:?}"),
    };
    assert_unmatched_terminal_replay_rejected(&store, key);
    let response = ControllerCommand::new(
        "command-suspended-response-driver",
        handle.clone(),
        1,
        suspended_revision,
        ControllerCommandVariant::HostInteractionResponse {
            interaction_id: request.interaction_id.clone(),
            logical_cycle: request.logical_cycle,
            operation_id: request.operation_id.clone(),
            tool_call_id: request.tool_call_id.clone(),
            request_digest: request.request_digest.clone(),
            response: HostInteractionMessage::user("Accepted after resume.").expect("response"),
        },
    )
    .expect("response command");
    let response_revision = match store
        .resolve_controller_command(response)
        .expect("response")
    {
        vv_agent::ControllerCommandResolution::Applied { receipt, wake } => {
            assert_eq!(wake.action, "none");
            receipt.resulting_revision
        }
        other => panic!("unexpected response resolution: {other:?}"),
    };
    let resume = ControllerCommand::new(
        "command-suspended-resume-driver",
        handle,
        1,
        response_revision,
        ControllerCommandVariant::Resume,
    )
    .expect("resume command");
    match store.resolve_controller_command(resume).expect("resume") {
        vv_agent::ControllerCommandResolution::Applied { receipt, wake } => {
            assert_eq!(receipt.resulting_status, "running");
            assert_eq!(wake.action, "recovery_dispatch");
        }
        other => panic!("unexpected resume resolution: {other:?}"),
    }
    let current = store
        .load_checkpoint(key)
        .expect("load resumed checkpoint")
        .expect("resumed checkpoint");
    let previous = envelope(&current, task(&current, 10), recipe(), 1);
    let registry = DistributedCapabilityRegistry::new();
    registry.register_checkpoint_store(checkpoint_ref(), store.clone());
    let enqueuer = Arc::new(RecordingEnqueuer::default());
    let backend = DistributedBackend::nonblocking(recipe(), registry, enqueuer.clone());
    let decision = backend
        .advance(
            &previous,
            DistributedDeliveryOutcome::worker(CycleDispatchResult::pending()),
        )
        .expect("advance suspended host resume");
    assert!(matches!(
        decision,
        DistributedAdvanceDecision::Dispatch { ref envelope, .. }
            if envelope.cycle_index == 1
                && envelope.claim_mode == ClaimMode::Recovery
                && envelope.resume_attempt == 1
    ));
    assert_eq!(enqueuer.deliveries().len(), 1);
    let recovered = store
        .load_checkpoint(key)
        .expect("load recovered checkpoint")
        .expect("recovered checkpoint");
    assert_eq!(recovered, current);
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

fn assert_unmatched_terminal_replay_rejected(store: &Arc<InMemoryCheckpointStore>, key: &str) {
    let checkpoint = store.load_checkpoint(key).unwrap().unwrap();
    let previous = envelope(&checkpoint, task(&checkpoint, 10), recipe(), 1);
    let registry = DistributedCapabilityRegistry::new();
    registry.register_checkpoint_store(checkpoint_ref(), store.clone());
    let enqueuer = Arc::new(RecordingEnqueuer::default());
    let backend = DistributedBackend::nonblocking(recipe(), registry, enqueuer.clone());
    let result = AgentResult::completed(Vec::new(), Vec::new(), "Uncommitted");
    let error = backend
        .advance(
            &previous,
            DistributedDeliveryOutcome::worker(
                CycleDispatchResult::terminal_replay(result, checkpoint.revision).unwrap(),
            ),
        )
        .expect_err("unmatched terminal replay");
    assert!(error.contains("no matching durable terminal"), "{error}");
    assert_eq!(
        store.load_checkpoint(key).unwrap().unwrap().revision,
        checkpoint.revision
    );
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

#[test]
fn direct_worker_rejects_brokered_approval_envelope_before_claim() {
    let checkpoint = minimal_checkpoint("worker-approval");
    let store = Arc::new(InMemoryCheckpointStore::new());
    store
        .create_checkpoint(checkpoint.clone())
        .expect("create checkpoint");
    let registry = DistributedCapabilityRegistry::new();
    registry.register_checkpoint_store(checkpoint_ref(), store.clone());
    let mut recipe = recipe();
    recipe.capabilities.approval_provider_ref =
        Some(CapabilityRef::new("approval.provider", "1").expect("provider ref"));
    recipe.capabilities.approval_broker_ref =
        Some(CapabilityRef::new("approval.broker", "1").expect("broker ref"));
    let envelope = envelope(&checkpoint, task(&checkpoint, 10), recipe, 1);

    let error = DistributedCycleWorker::new(registry)
        .run_cycle(envelope)
        .expect_err("direct worker approval rejection");

    assert!(error.contains("do not support brokered approval waits"));
    let persisted = store
        .load_checkpoint("worker-approval")
        .expect("load checkpoint")
        .expect("checkpoint");
    assert_eq!(persisted.revision, 0);
    assert!(persisted.claim_token.is_none());
}
