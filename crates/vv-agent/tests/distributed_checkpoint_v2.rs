use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use vv_agent::runtime::backends::distributed::{
    CapabilityRef, DistributedCapabilities, DistributedCapabilityRegistry,
    DistributedCheckpointConfig, DistributedCheckpointExtensionRef, DistributedCheckpointProgress,
    DistributedCycleWorker, DistributedDeliveryMetadata, DistributedRunEnvelope,
    DistributedV2CycleExecutor, DistributedV2CycleOutcome, ResolvedDistributedCapabilities,
    DEFAULT_CYCLE_NAME, DISTRIBUTED_RUN_SCHEMA_VERSION_V1,
};
use vv_agent::runtime::checkpoint_codec_v2::checkpoint_v2_from_value;
use vv_agent::{
    AgentResult, AgentTask, AmbiguousModelPolicy, AmbiguousToolPolicy, CheckpointStatus,
    CheckpointStoreV2, ClaimMode, EventOutboxEntry, InMemoryCheckpointStoreV2,
    InMemoryRunEventStore, ModelSettings, OperationJournalEntry, OperationState, ResumePolicy,
    RunBudgetLimits, RuntimeRecipe, ToolIdempotency,
};

const V1_FIXTURE: &str = include_str!("fixtures/parity/distributed_run_envelope_v1.json");
const V2_FIXTURE: &str = include_str!("fixtures/parity/distributed_run_envelope_v2.json");
const CODEC_FIXTURE: &str = include_str!("fixtures/parity/checkpoint_codec_v2.json");
const JOURNAL_FIXTURE: &str = include_str!("fixtures/parity/operation_journal_v1.json");

type ExecutorFn = dyn FnMut(
        &DistributedRunEnvelope,
        &ResolvedDistributedCapabilities,
        &mut DistributedCheckpointProgress,
    ) -> Result<DistributedV2CycleOutcome, String>
    + Send;

struct TestExecutor {
    handler: Mutex<Box<ExecutorFn>>,
}

impl TestExecutor {
    fn new(
        handler: impl FnMut(
                &DistributedRunEnvelope,
                &ResolvedDistributedCapabilities,
                &mut DistributedCheckpointProgress,
            ) -> Result<DistributedV2CycleOutcome, String>
            + Send
            + 'static,
    ) -> Self {
        Self {
            handler: Mutex::new(Box::new(handler)),
        }
    }
}

impl DistributedV2CycleExecutor for TestExecutor {
    fn execute(
        &self,
        envelope: &DistributedRunEnvelope,
        capabilities: &ResolvedDistributedCapabilities,
        checkpoint: &mut DistributedCheckpointProgress,
    ) -> Result<DistributedV2CycleOutcome, String> {
        (self.handler.lock().expect("executor handler"))(envelope, capabilities, checkpoint)
    }
}

fn fixture(raw: &str) -> Value {
    serde_json::from_str(raw).expect("valid fixture")
}

fn minimal_checkpoint(
    key: &str,
    task_id: &str,
    root_run_id: &str,
    trace_id: &str,
) -> vv_agent::CheckpointV2 {
    let mut payload = fixture(CODEC_FIXTURE)["valid_cases"]
        .as_array()
        .expect("valid cases")
        .iter()
        .find(|case| case["name"] == "minimal_running")
        .expect("minimal checkpoint")["payload"]
        .clone();
    payload["checkpoint_key"] = json!(key);
    payload["task_id"] = json!(task_id);
    payload["root_run_id"] = json!(root_run_id);
    payload["trace_id"] = json!(trace_id);
    checkpoint_v2_from_value(&payload, 262_144).expect("valid minimal checkpoint")
}

fn journal_entry(name: &str) -> OperationJournalEntry {
    let entry = fixture(JOURNAL_FIXTURE)["valid_entries"]
        .as_array()
        .expect("journal entries")
        .iter()
        .find(|entry| entry["name"] == name)
        .unwrap_or_else(|| panic!("missing journal entry {name}"))["entry"]
        .clone();
    OperationJournalEntry::from_value(&entry).expect("valid journal entry")
}

fn store_ref() -> CapabilityRef {
    CapabilityRef::new("checkpoint.test", "2").unwrap()
}

fn event_store_ref() -> CapabilityRef {
    CapabilityRef::new("events.test", "2").unwrap()
}

fn envelope(
    checkpoint: &vv_agent::CheckpointV2,
    cycle_index: u32,
    claim_mode: ClaimMode,
    lease_duration_ms: u64,
    include_event_store: bool,
) -> DistributedRunEnvelope {
    let mut task = AgentTask::new(
        checkpoint.task_id.clone(),
        "test-model",
        "You are a careful assistant.",
        "Summarize the status.",
    );
    task.max_cycles = 10;
    task.use_workspace = false;
    task.exclude_tools = vec!["task_finish".to_string(), "ask_user".to_string()];
    task.metadata.insert(
        "_vv_agent_run_id".to_string(),
        json!(checkpoint.root_run_id),
    );
    let mut recipe = RuntimeRecipe::new("settings.json", "test", "test-model", ".");
    recipe.capabilities = DistributedCapabilities {
        checkpoint_store_ref: Some(store_ref()),
        checkpoint_event_store_ref: include_event_store.then(event_store_ref),
        ..DistributedCapabilities::default()
    };
    DistributedRunEnvelope::for_checkpoint_cycle(
        task,
        recipe,
        cycle_index,
        DEFAULT_CYCLE_NAME,
        Some(checkpoint.root_run_id.clone()),
        None,
        lease_duration_ms,
        None,
        checkpoint.root_run_id.clone(),
        checkpoint.trace_id.clone(),
        checkpoint.run_definition_digest.clone(),
        claim_mode,
        checkpoint.resume_attempt,
        DistributedCheckpointConfig {
            key: checkpoint.checkpoint_key.clone(),
            resume_policy: ResumePolicy::RequireExisting,
            ambiguous_model_policy: AmbiguousModelPolicy::RequireReconciliation,
            ambiguous_tool_policy: AmbiguousToolPolicy::RequireReconciliation,
            required_extension_namespaces: Vec::new(),
            max_extension_state_bytes: 262_144,
            credential_slots: Vec::new(),
        },
    )
    .unwrap()
}

fn registry_with_store(
    store: Arc<InMemoryCheckpointStoreV2>,
    event_store: Option<Arc<InMemoryRunEventStore>>,
) -> DistributedCapabilityRegistry {
    let registry = DistributedCapabilityRegistry::new();
    registry.register_checkpoint_store(store_ref(), store);
    if let Some(event_store) = event_store {
        registry.register_checkpoint_event_store(event_store_ref(), event_store);
    }
    registry
}

fn set_path(payload: &mut Value, path: &[Value], value: Value) {
    let mut target = payload;
    for key in &path[..path.len() - 1] {
        target = &mut target[key.as_str().expect("path key")];
    }
    target[path.last().and_then(Value::as_str).expect("final path key")] = value;
}

#[test]
fn distributed_discriminator_round_trips_v2_and_preserves_v1_wire() {
    let v1 = fixture(V1_FIXTURE);
    let v1_canonical = v1["canonical_envelope"].clone();
    let v1_envelope = DistributedRunEnvelope::from_dict(&v1_canonical).unwrap();
    assert_eq!(
        v1_envelope.schema_version,
        DISTRIBUTED_RUN_SCHEMA_VERSION_V1
    );
    assert_eq!(v1_envelope.to_dict(), v1_canonical);
    assert_eq!(serde_json::to_value(&v1_envelope).unwrap(), v1_canonical);
    assert_eq!(
        format!("{:x}", Sha256::digest(V1_FIXTURE.as_bytes())),
        "c1eb11591c93e8ac880fd4688cf06e0fe60a8b4522f7707ea13e1cccf40208e0"
    );

    let v2 = fixture(V2_FIXTURE);
    let v2_canonical = v2["canonical_envelope"].clone();
    let v2_envelope = DistributedRunEnvelope::from_dict(&v2_canonical).unwrap();
    assert!(v2_envelope.is_checkpoint_v2());
    assert_eq!(v2_envelope.to_dict(), v2_canonical);

    for case in v2["invalid_cases"].as_array().unwrap() {
        if matches!(
            case["name"].as_str(),
            Some("definition_digest_mismatch" | "resume_attempt_mismatch")
        ) {
            continue;
        }
        let mut payload = v2_canonical.clone();
        set_path(
            &mut payload,
            case["path"].as_array().unwrap(),
            case["value"].clone(),
        );
        let error = DistributedRunEnvelope::from_dict(&payload).unwrap_err();
        assert!(
            error.contains(case["error"].as_str().unwrap()),
            "case {} returned {error}",
            case["name"]
        );
    }
}

#[test]
fn v2_worker_resolves_every_capability_before_claim() {
    let store = Arc::new(InMemoryCheckpointStoreV2::new());
    let checkpoint = minimal_checkpoint("capability-first", "task-cap", "run-cap", "trace-cap");
    store.create_checkpoint_v2(checkpoint.clone()).unwrap();
    let registry = registry_with_store(store.clone(), None);
    let mut envelope = envelope(&checkpoint, 1, ClaimMode::Continue, 1_000, false);
    envelope.recipe.capabilities.checkpoint_extension_refs.push(
        DistributedCheckpointExtensionRef {
            namespace: "com.example.optional".to_string(),
            reference: CapabilityRef::new("extension.missing", "1").unwrap(),
            required: false,
        },
    );

    let error = DistributedCycleWorker::new(registry)
        .run_cycle(envelope)
        .unwrap_err();

    assert_eq!(
        error,
        "unknown distributed capability checkpoint_extension extension.missing@1"
    );
    let persisted = store
        .load_checkpoint_v2("capability-first")
        .unwrap()
        .unwrap();
    assert_eq!(persisted.revision, 0);
    assert!(persisted.claim_token.is_none());
}

#[test]
fn v2_live_claim_redelivery_does_not_steal_or_increment_attempt() {
    let store = Arc::new(InMemoryCheckpointStoreV2::new());
    let mut checkpoint = minimal_checkpoint("live-claim", "task-live", "run-live", "trace-live");
    checkpoint.revision = 1;
    checkpoint.claim_token = Some("owner-live".to_string());
    checkpoint.claimed_cycle = Some(1);
    checkpoint.lease_expires_at_ms =
        Some(u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap() + 60_000);
    store.create_checkpoint_v2(checkpoint.clone()).unwrap();
    let registry = registry_with_store(store.clone(), None);

    let dispatch = DistributedCycleWorker::new(registry)
        .run_cycle_with_delivery(
            envelope(&checkpoint, 1, ClaimMode::Continue, 1_000, false),
            DistributedDeliveryMetadata::redelivery(2),
        )
        .unwrap();

    assert!(!dispatch.finished);
    assert!(!dispatch.terminal_candidate);
    let persisted = store.load_checkpoint_v2("live-claim").unwrap().unwrap();
    assert_eq!(persisted.revision, 1);
    assert_eq!(persisted.resume_attempt, 1);
    assert_eq!(persisted.claim_token.as_deref(), Some("owner-live"));
}

#[test]
fn v2_expired_started_unknown_tool_suspends_for_reconciliation() {
    let store = Arc::new(InMemoryCheckpointStoreV2::new());
    let mut checkpoint = minimal_checkpoint(
        "ambiguous-tool",
        "task-ambiguous",
        "run-ambiguous",
        "trace-ambiguous",
    );
    let mut started = journal_entry("tool_started");
    started.idempotency_support = Some(ToolIdempotency::Unknown);
    checkpoint.tool_journal.push(started);
    checkpoint.revision = 1;
    checkpoint.claim_token = Some("expired-owner".to_string());
    checkpoint.claimed_cycle = Some(1);
    checkpoint.lease_expires_at_ms = Some(1);
    store.create_checkpoint_v2(checkpoint.clone()).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_executor = calls.clone();
    let executor = TestExecutor::new(move |_, _, _| {
        calls_for_executor.fetch_add(1, Ordering::SeqCst);
        unreachable!("ambiguous unknown tool must not reach execution")
    });
    let registry = registry_with_store(store.clone(), None);

    let dispatch = DistributedCycleWorker::new(registry)
        .with_checkpoint_executor(Arc::new(executor))
        .run_cycle_with_delivery(
            envelope(&checkpoint, 1, ClaimMode::Continue, 1_000, false),
            DistributedDeliveryMetadata::redelivery(2),
        )
        .unwrap();

    assert!(dispatch.finished);
    assert!(dispatch.terminal_candidate);
    assert_eq!(
        dispatch.result.as_ref().map(|result| result.status),
        Some(vv_agent::AgentStatus::ReconciliationRequired)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let persisted = store.load_checkpoint_v2("ambiguous-tool").unwrap().unwrap();
    assert_eq!(persisted.status, CheckpointStatus::ReconciliationRequired);
    assert_eq!(persisted.resume_attempt, 2);
    assert_eq!(persisted.tool_journal[0].state, OperationState::Ambiguous);
    assert!(persisted.claim_token.is_none());
}

#[test]
fn v2_redelivery_replays_committed_receipt_without_external_call() {
    let store = Arc::new(InMemoryCheckpointStoreV2::new());
    let mut checkpoint = minimal_checkpoint(
        "receipt-replay",
        "task-replay",
        "run-replay",
        "trace-replay",
    );
    checkpoint
        .model_call_journal
        .push(journal_entry("model_succeeded"));
    store.create_checkpoint_v2(checkpoint.clone()).unwrap();
    let external_calls = Arc::new(AtomicUsize::new(0));
    let executor = TestExecutor::new(move |envelope, _, progress| {
        assert_eq!(
            progress.checkpoint().model_call_journal[0].state,
            OperationState::Succeeded
        );
        let mut committed = progress.checkpoint().clone();
        committed.cycle_index = u64::from(envelope.cycle_index);
        Ok(DistributedV2CycleOutcome::Continue(committed))
    });
    let registry = registry_with_store(store.clone(), None);

    let dispatch = DistributedCycleWorker::new(registry)
        .with_checkpoint_executor(Arc::new(executor))
        .run_cycle_with_delivery(
            envelope(&checkpoint, 1, ClaimMode::Continue, 1_000, false),
            DistributedDeliveryMetadata::redelivery(2),
        )
        .unwrap();

    assert!(!dispatch.finished);
    assert_eq!(external_calls.load(Ordering::SeqCst), 0);
    let persisted = store.load_checkpoint_v2("receipt-replay").unwrap().unwrap();
    assert_eq!(persisted.cycle_index, 1);
    assert_eq!(persisted.resume_attempt, 2);
    assert!(persisted.model_call_journal.is_empty());
}

#[test]
fn v2_idempotent_retry_reuses_key_and_committed_cycle_absorbs_stale_redelivery() {
    let store = Arc::new(InMemoryCheckpointStoreV2::new());
    let mut checkpoint = minimal_checkpoint(
        "idempotent-retry",
        "task-idempotent",
        "run-idempotent",
        "trace-idempotent",
    );
    let started = journal_entry("tool_started");
    let original_key = started.idempotency_key.clone();
    checkpoint.tool_journal.push(started);
    checkpoint.run_definition["checkpoint_policy"]["ambiguous_tool_policy"] =
        json!("retry_idempotent_only");
    checkpoint.run_definition_digest =
        vv_agent::run_definition_digest(&checkpoint.run_definition).unwrap();
    checkpoint.revision = 1;
    checkpoint.claim_token = Some("expired-idempotent-owner".to_string());
    checkpoint.claimed_cycle = Some(1);
    checkpoint.lease_expires_at_ms = Some(1);
    store.create_checkpoint_v2(checkpoint.clone()).unwrap();
    let external_calls = Arc::new(AtomicUsize::new(0));
    let external_calls_for_executor = external_calls.clone();
    let executor = TestExecutor::new(move |envelope, _, progress| {
        let durable = &progress.checkpoint().tool_journal[0];
        assert_eq!(durable.state, OperationState::Planned);
        assert_eq!(durable.attempt, 2);
        assert_eq!(durable.idempotency_key, original_key);

        let mut started = progress.checkpoint().clone();
        started.tool_journal[0]
            .transition_to(OperationState::Started)
            .map_err(|error| error.to_string())?;
        progress.persist(started)?;
        external_calls_for_executor.fetch_add(1, Ordering::SeqCst);

        let mut succeeded = progress.checkpoint().clone();
        succeeded.tool_journal[0].result = Some(json!({"receipt_id": "receipt-1"}));
        succeeded.tool_journal[0]
            .transition_to(OperationState::Succeeded)
            .map_err(|error| error.to_string())?;
        progress.persist(succeeded)?;

        let mut committed = progress.checkpoint().clone();
        committed.cycle_index = u64::from(envelope.cycle_index);
        Ok(DistributedV2CycleOutcome::Continue(committed))
    });
    let registry = registry_with_store(store.clone(), None);
    let worker = DistributedCycleWorker::new(registry).with_checkpoint_executor(Arc::new(executor));
    let mut stale_envelope = envelope(&checkpoint, 1, ClaimMode::Continue, 1_000, false);
    stale_envelope
        .checkpoint_config
        .as_mut()
        .unwrap()
        .ambiguous_tool_policy = AmbiguousToolPolicy::RetryIdempotentOnly;

    let first = worker
        .run_cycle_with_delivery(
            stale_envelope.clone(),
            DistributedDeliveryMetadata::redelivery(2),
        )
        .unwrap();
    assert!(!first.finished);
    assert_eq!(external_calls.load(Ordering::SeqCst), 1);

    let stale_redelivery = worker
        .run_cycle_with_delivery(stale_envelope, DistributedDeliveryMetadata::redelivery(3))
        .unwrap();
    assert!(!stale_redelivery.finished);
    assert_eq!(external_calls.load(Ordering::SeqCst), 1);
    let persisted = store
        .load_checkpoint_v2("idempotent-retry")
        .unwrap()
        .unwrap();
    assert_eq!(persisted.cycle_index, 1);
    assert_eq!(persisted.resume_attempt, 2);
    assert!(persisted.tool_journal.is_empty());
}

#[test]
fn v2_heartbeat_does_not_overwrite_progress_revision_or_journal() {
    let store = Arc::new(InMemoryCheckpointStoreV2::new());
    let checkpoint = minimal_checkpoint(
        "heartbeat-progress",
        "task-heartbeat",
        "run-heartbeat",
        "trace-heartbeat",
    );
    store.create_checkpoint_v2(checkpoint.clone()).unwrap();
    let store_for_executor = store.clone();
    let executor = TestExecutor::new(move |envelope, _, progress| {
        let mut planned = progress.checkpoint().clone();
        planned
            .model_call_journal
            .push(journal_entry("model_planned"));
        let progressed = progress.persist(planned)?;
        let first_expiry = progressed.lease_expires_at_ms.unwrap();
        std::thread::sleep(Duration::from_millis(180));
        let after_heartbeat = store_for_executor
            .load_checkpoint_v2("heartbeat-progress")
            .map_err(|error| error.to_string())?
            .unwrap();
        assert_eq!(after_heartbeat.revision, progressed.revision);
        assert_eq!(after_heartbeat.model_call_journal.len(), 1);
        assert!(after_heartbeat.lease_expires_at_ms.unwrap() > first_expiry);
        let mut committed = after_heartbeat;
        committed.cycle_index = u64::from(envelope.cycle_index);
        Ok(DistributedV2CycleOutcome::Continue(committed))
    });
    let registry = registry_with_store(store.clone(), None);

    DistributedCycleWorker::new(registry)
        .with_checkpoint_executor(Arc::new(executor))
        .run_cycle(envelope(&checkpoint, 1, ClaimMode::Continue, 300, false))
        .unwrap();

    let persisted = store
        .load_checkpoint_v2("heartbeat-progress")
        .unwrap()
        .unwrap();
    assert_eq!(persisted.revision, 3);
    assert_eq!(persisted.cycle_index, 1);
    assert!(persisted.model_call_journal.is_empty());
}

#[test]
fn v2_terminal_candidate_retains_claim_without_finalizing_or_acknowledging() {
    let store = Arc::new(InMemoryCheckpointStoreV2::new());
    let event_store = Arc::new(InMemoryRunEventStore::default());
    let checkpoint = minimal_checkpoint(
        "terminal-two-phase",
        "task-terminal",
        "run-terminal",
        "trace-terminal",
    );
    store.create_checkpoint_v2(checkpoint.clone()).unwrap();
    let executor_calls = Arc::new(AtomicUsize::new(0));
    let executor_calls_for_handler = executor_calls.clone();
    let executor = TestExecutor::new(move |envelope, _, progress| {
        executor_calls_for_handler.fetch_add(1, Ordering::SeqCst);
        let mut terminal = progress.checkpoint().clone();
        terminal.cycle_index = u64::from(envelope.cycle_index);
        terminal.status = CheckpointStatus::Completed;
        let mut result = AgentResult::completed(Vec::new(), Vec::new(), "done");
        result.checkpoint_key = Some("terminal-two-phase".to_string());
        terminal.terminal_result = Some(result.to_dict());
        terminal.event_outbox.push(
            EventOutboxEntry::pending(
                "evt-terminal-two-phase",
                json!({"type": "run_completed", "status": "completed"}),
            )
            .unwrap(),
        );
        Ok(DistributedV2CycleOutcome::Terminal(terminal))
    });
    let registry = registry_with_store(store.clone(), Some(event_store));
    let worker = DistributedCycleWorker::new(registry).with_checkpoint_executor(Arc::new(executor));
    let terminal_envelope = envelope(&checkpoint, 1, ClaimMode::Continue, 1_000, true);

    let dispatch = worker.run_cycle(terminal_envelope.clone()).unwrap();

    assert!(dispatch.finished);
    assert!(dispatch.terminal_candidate);
    assert!(!dispatch.terminal_replay);
    assert_eq!(executor_calls.load(Ordering::SeqCst), 1);
    let persisted = store
        .load_checkpoint_v2("terminal-two-phase")
        .unwrap()
        .unwrap();
    assert_eq!(dispatch.checkpoint_revision, Some(persisted.revision));
    assert_eq!(persisted.cycle_index, 0);
    assert_eq!(persisted.status, CheckpointStatus::Running);
    assert!(!persisted.terminal_acknowledged);
    assert!(persisted.terminal_result.is_none());
    assert!(persisted.event_outbox.is_empty());
    assert!(persisted.claim_token.is_some());

    let replay = worker
        .run_cycle_with_delivery(
            terminal_envelope,
            DistributedDeliveryMetadata::redelivery(2),
        )
        .unwrap();
    assert!(!replay.finished);
    assert!(!replay.terminal_candidate);
    assert_eq!(executor_calls.load(Ordering::SeqCst), 1);
    assert!(store
        .load_checkpoint_v2("terminal-two-phase")
        .unwrap()
        .is_some());
}

#[test]
fn v2_definition_and_resume_attempt_mismatch_fail_before_claim() {
    let store = Arc::new(InMemoryCheckpointStoreV2::new());
    let checkpoint = minimal_checkpoint(
        "identity-mismatch",
        "task-identity",
        "run-identity",
        "trace-identity",
    );
    store.create_checkpoint_v2(checkpoint.clone()).unwrap();
    let registry = registry_with_store(store.clone(), None);
    let worker = DistributedCycleWorker::new(registry);

    let mut wrong_definition = envelope(&checkpoint, 1, ClaimMode::Continue, 1_000, false);
    wrong_definition.run_definition_digest = Some("d".repeat(64));
    assert_eq!(
        worker.run_cycle(wrong_definition).unwrap_err(),
        "checkpoint_definition_mismatch"
    );

    let mut wrong_task = envelope(&checkpoint, 1, ClaimMode::Continue, 1_000, false);
    wrong_task.task.system_prompt = "tampered prompt".to_string();
    assert!(worker
        .run_cycle(wrong_task)
        .unwrap_err()
        .contains("checkpoint_definition_mismatch"));

    let mut wrong_budget = envelope(&checkpoint, 1, ClaimMode::Continue, 1_000, false);
    wrong_budget.budget_limits = Some(
        RunBudgetLimits::builder()
            .max_total_tokens(10)
            .build()
            .unwrap(),
    );
    assert!(worker
        .run_cycle(wrong_budget)
        .unwrap_err()
        .contains("checkpoint_definition_mismatch"));

    let mut wrong_policy = envelope(&checkpoint, 1, ClaimMode::Continue, 1_000, false);
    wrong_policy.recipe.capabilities.tool_policy.allowed_tools = Some(Vec::new());
    assert!(worker
        .run_cycle(wrong_policy)
        .unwrap_err()
        .contains("checkpoint_definition_mismatch"));

    let mut wrong_attempt = envelope(&checkpoint, 1, ClaimMode::Continue, 1_000, false);
    wrong_attempt.resume_attempt = Some(2);
    assert_eq!(
        worker.run_cycle(wrong_attempt).unwrap_err(),
        "checkpoint_resume_attempt_mismatch"
    );
    let persisted = store
        .load_checkpoint_v2("identity-mismatch")
        .unwrap()
        .unwrap();
    assert_eq!(persisted.revision, 0);
    assert!(persisted.claim_token.is_none());
}

#[test]
fn v2_definition_validation_redacts_credentials_and_normalizes_tool_policy_sets() {
    let store = Arc::new(InMemoryCheckpointStoreV2::new());
    let mut checkpoint = minimal_checkpoint(
        "normalized-definition",
        "task-normalized",
        "run-normalized",
        "trace-normalized",
    );
    checkpoint.run_definition["credential_slots"] =
        json!(["/model/settings/extra_headers/authorization"]);
    checkpoint.run_definition["model"]["settings"] = json!({
        "extra_headers": {
            "authorization": vv_agent::checkpoint::CREDENTIAL_REDACTED,
        },
    });
    checkpoint.run_definition["tool_policy"]["allowed_tools"] = json!(["alpha", "beta"]);
    checkpoint.run_definition["tool_policy"]["disallowed_tools"] =
        json!(["blocked-a", "blocked-b"]);
    checkpoint.run_definition_digest =
        vv_agent::run_definition_digest(&checkpoint.run_definition).unwrap();
    store.create_checkpoint_v2(checkpoint.clone()).unwrap();

    let executor = TestExecutor::new(move |envelope, _, progress| {
        let mut committed = progress.checkpoint().clone();
        committed.cycle_index = u64::from(envelope.cycle_index);
        Ok(DistributedV2CycleOutcome::Continue(committed))
    });
    let registry = registry_with_store(store.clone(), None);
    let worker = DistributedCycleWorker::new(registry).with_checkpoint_executor(Arc::new(executor));
    let mut envelope = envelope(&checkpoint, 1, ClaimMode::Continue, 1_000, false);
    envelope
        .checkpoint_config
        .as_mut()
        .unwrap()
        .credential_slots = vec!["/model/settings/extra_headers/authorization".to_string()];
    envelope.task.model_settings = Some(
        ModelSettings::builder()
            .extra_header("Authorization", "live-secret")
            .build(),
    );
    envelope.recipe.capabilities.tool_policy.allowed_tools = Some(vec![
        "beta".to_string(),
        "alpha".to_string(),
        "alpha".to_string(),
    ]);
    envelope.recipe.capabilities.tool_policy.disallowed_tools = vec![
        "blocked-b".to_string(),
        "blocked-a".to_string(),
        "blocked-b".to_string(),
    ];

    let dispatch = worker.run_cycle(envelope).unwrap();

    assert!(!dispatch.finished);
    assert_eq!(dispatch.committed_cycle, Some(1));
    let persisted = store
        .load_checkpoint_v2("normalized-definition")
        .unwrap()
        .unwrap();
    assert_eq!(persisted.cycle_index, 1);
}
