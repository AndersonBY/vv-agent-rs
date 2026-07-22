use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use vv_agent::runtime::backends::distributed::{
    CapabilityRef, DistributedCapabilities, DistributedCapabilityRegistry,
    DistributedCheckpointConfig, DistributedCheckpointExtensionRef, DistributedCheckpointProgress,
    DistributedCycleExecutor, DistributedCycleOutcome, DistributedCycleWorker,
    DistributedDeliveryMetadata, DistributedRunEnvelope, ResolvedDistributedCapabilities,
    DEFAULT_CYCLE_NAME,
};
use vv_agent::runtime::checkpoint_codec::checkpoint_from_value;
use vv_agent::types::AgentTask;
use vv_agent::{
    AfterCycleDecision, AfterCycleHook, AfterCycleSnapshot, AgentResult, AmbiguousModelPolicy,
    AmbiguousToolPolicy, CheckpointExtension, CheckpointStatus, CheckpointStore, ClaimMode,
    CycleDispatchResult, EventOutboxEntry, ExtensionStateEntry, InMemoryCheckpointStore,
    InMemoryRunEventStore, LLMResponse, ModelSettings, OperationJournalEntry, OperationState,
    ResumePolicy, RunBudgetLimits, RuntimeRecipe, ScriptedLlmClient, ToolIdempotency,
};

const ENVELOPE_FIXTURE: &str = include_str!("fixtures/parity/distributed_run_envelope.json");
const CODEC_FIXTURE: &str = include_str!("fixtures/parity/checkpoint_codec.json");
const JOURNAL_FIXTURE: &str = include_str!("fixtures/parity/operation_journal.json");

type ExecutorFn = dyn FnMut(
        &DistributedRunEnvelope,
        &ResolvedDistributedCapabilities,
        &mut DistributedCheckpointProgress,
    ) -> Result<DistributedCycleOutcome, String>
    + Send;

struct TestExecutor {
    handler: Mutex<Box<ExecutorFn>>,
}

#[derive(Default)]
struct StatefulAfterCycleHook {
    observed_cycles: AtomicUsize,
    restored_values: Mutex<Vec<usize>>,
}

impl AfterCycleHook for StatefulAfterCycleHook {
    fn after_cycle(
        &self,
        _snapshot: &AfterCycleSnapshot,
    ) -> Result<Option<AfterCycleDecision>, String> {
        self.observed_cycles.fetch_add(1, Ordering::SeqCst);
        Ok(Some(AfterCycleDecision::continue_run()))
    }
}

impl CheckpointExtension for StatefulAfterCycleHook {
    fn namespace(&self) -> &str {
        "com.example.lifecycle"
    }

    fn version(&self) -> &str {
        "1"
    }

    fn required(&self) -> bool {
        true
    }

    fn snapshot(&self) -> vv_agent::checkpoint::CheckpointResult<Value> {
        Ok(json!({
            "observed_cycles": self.observed_cycles.load(Ordering::SeqCst),
        }))
    }

    fn restore(&self, state: &Value) -> vv_agent::checkpoint::CheckpointResult<()> {
        let value = state
            .get("observed_cycles")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                vv_agent::CheckpointError::new(
                    "checkpoint_extension_state_invalid",
                    "observed_cycles is missing",
                )
            })?;
        let value = usize::try_from(value).map_err(|_| {
            vv_agent::CheckpointError::new(
                "checkpoint_extension_state_invalid",
                "observed_cycles exceeds usize",
            )
        })?;
        self.observed_cycles.store(value, Ordering::SeqCst);
        self.restored_values
            .lock()
            .expect("restored values")
            .push(value);
        Ok(())
    }
}

impl TestExecutor {
    fn new(
        handler: impl FnMut(
                &DistributedRunEnvelope,
                &ResolvedDistributedCapabilities,
                &mut DistributedCheckpointProgress,
            ) -> Result<DistributedCycleOutcome, String>
            + Send
            + 'static,
    ) -> Self {
        Self {
            handler: Mutex::new(Box::new(handler)),
        }
    }
}

impl DistributedCycleExecutor for TestExecutor {
    fn execute(
        &self,
        envelope: &DistributedRunEnvelope,
        capabilities: &ResolvedDistributedCapabilities,
        checkpoint: &mut DistributedCheckpointProgress,
    ) -> Result<DistributedCycleOutcome, String> {
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
) -> vv_agent::Checkpoint {
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
    checkpoint_from_value(&payload, 262_144).expect("valid minimal checkpoint")
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
    checkpoint: &vv_agent::Checkpoint,
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
    task.memory_compact_threshold = checkpoint.run_definition["runtime_controls"]
        ["memory_compact_threshold"]
        .as_u64()
        .expect("durable memory compact threshold");
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
    DistributedRunEnvelope::for_cycle(
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
    store: Arc<InMemoryCheckpointStore>,
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
fn distributed_envelope_accepts_only_the_current_wire_shape() {
    let contract = fixture(ENVELOPE_FIXTURE);
    let canonical = contract["canonical_envelope"].clone();
    let envelope = DistributedRunEnvelope::from_dict(&canonical).unwrap();
    assert_eq!(envelope.to_dict(), canonical);
    assert_eq!(serde_json::to_value(&envelope).unwrap(), canonical);

    for case in contract["invalid_cases"].as_array().unwrap() {
        if matches!(
            case["name"].as_str(),
            Some(
                "definition_digest_mismatch"
                    | "resume_attempt_mismatch"
                    | "missing_after_cycle_hook_ref"
            )
        ) {
            continue;
        }
        let mut payload = canonical.clone();
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
fn missing_after_cycle_hook_fails_before_claim() {
    let store = Arc::new(InMemoryCheckpointStore::new());
    let checkpoint = minimal_checkpoint(
        "missing-lifecycle",
        "task-lifecycle",
        "run-lifecycle",
        "trace-lifecycle",
    );
    store.create_checkpoint(checkpoint.clone()).unwrap();
    let registry = registry_with_store(store.clone(), None);
    let mut envelope = envelope(&checkpoint, 1, ClaimMode::Continue, 1_000, false);
    envelope
        .recipe
        .capabilities
        .after_cycle_hook_refs
        .push(CapabilityRef::new("lifecycle.missing", "1").unwrap());

    let error = DistributedCycleWorker::new(registry)
        .run_cycle(envelope)
        .unwrap_err();

    assert_eq!(
        error,
        "unknown distributed capability after_cycle_hook lifecycle.missing@1"
    );
    let persisted = store.load_checkpoint("missing-lifecycle").unwrap().unwrap();
    assert_eq!(persisted.revision, 0);
    assert!(persisted.claim_token.is_none());
}

#[test]
fn worker_restores_stateful_after_cycle_hook_before_next_cycle() {
    let store = Arc::new(InMemoryCheckpointStore::new());
    let mut checkpoint = minimal_checkpoint(
        "stateful-lifecycle",
        "task-stateful-lifecycle",
        "run-stateful-lifecycle",
        "trace-stateful-lifecycle",
    );
    checkpoint.cycle_index = 1;
    checkpoint.run_definition["extensions"] = json!([{
        "namespace": "com.example.lifecycle",
        "version": "1",
        "required": true,
    }]);
    checkpoint.run_definition["capability_refs"]["after_cycle_hook:0"] =
        json!({"id": "lifecycle.policy", "version": "1"});
    checkpoint.extension_state.insert(
        "com.example.lifecycle".to_string(),
        ExtensionStateEntry {
            version: "1".to_string(),
            required: true,
            state: json!({"observed_cycles": 1}),
        },
    );
    checkpoint.run_definition_digest =
        vv_agent::run_definition_digest(&checkpoint.run_definition).unwrap();
    store.create_checkpoint(checkpoint.clone()).unwrap();

    let hook_ref = CapabilityRef::new("lifecycle.policy", "1").unwrap();
    let extension_ref = CapabilityRef::new("lifecycle.policy-state", "1").unwrap();
    let llm_ref = CapabilityRef::new("llm.scripted", "1").unwrap();
    let hook = Arc::new(StatefulAfterCycleHook::default());
    let registry = registry_with_store(store.clone(), None);
    registry.register_after_cycle_hook(hook_ref.clone(), hook.clone());
    registry.register_checkpoint_extension(extension_ref.clone(), hook.clone());
    registry.register_llm_client(
        llm_ref.clone(),
        Arc::new(ScriptedLlmClient::new(vec![LLMResponse::new("cycle two")])),
    );

    let mut envelope = envelope(&checkpoint, 2, ClaimMode::Continue, 60_000, false);
    envelope.recipe.capabilities.llm_client_ref = Some(llm_ref);
    envelope.recipe.capabilities.after_cycle_hook_refs = vec![hook_ref];
    envelope.recipe.capabilities.checkpoint_extension_refs.push(
        DistributedCheckpointExtensionRef {
            namespace: "com.example.lifecycle".to_string(),
            reference: extension_ref,
            required: true,
        },
    );
    envelope.checkpoint_config.required_extension_namespaces =
        vec!["com.example.lifecycle".to_string()];

    let dispatch = DistributedCycleWorker::new(registry)
        .run_cycle(envelope)
        .expect("distributed cycle");

    assert!(matches!(dispatch, CycleDispatchResult::Committed { .. }));
    assert_eq!(
        hook.restored_values
            .lock()
            .expect("restored values")
            .as_slice(),
        [1]
    );
    assert_eq!(hook.observed_cycles.load(Ordering::SeqCst), 2);
    let persisted = store
        .load_checkpoint("stateful-lifecycle")
        .unwrap()
        .unwrap();
    assert_eq!(persisted.cycle_index, 2);
    assert_eq!(
        persisted.extension_state["com.example.lifecycle"].state,
        json!({"observed_cycles": 2})
    );
}

#[test]
fn worker_resolves_every_capability_before_claim() {
    let store = Arc::new(InMemoryCheckpointStore::new());
    let checkpoint = minimal_checkpoint("capability-first", "task-cap", "run-cap", "trace-cap");
    store.create_checkpoint(checkpoint.clone()).unwrap();
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
    let persisted = store.load_checkpoint("capability-first").unwrap().unwrap();
    assert_eq!(persisted.revision, 0);
    assert!(persisted.claim_token.is_none());
}

#[test]
fn live_claim_redelivery_does_not_steal_or_increment_attempt() {
    let store = Arc::new(InMemoryCheckpointStore::new());
    let mut checkpoint = minimal_checkpoint("live-claim", "task-live", "run-live", "trace-live");
    checkpoint.revision = 1;
    checkpoint.claim_token = Some("owner-live".to_string());
    checkpoint.claimed_cycle = Some(1);
    checkpoint.lease_expires_at_ms =
        Some(u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap() + 60_000);
    store.create_checkpoint(checkpoint.clone()).unwrap();
    let registry = registry_with_store(store.clone(), None);

    let dispatch = DistributedCycleWorker::new(registry)
        .run_cycle_with_delivery(
            envelope(&checkpoint, 1, ClaimMode::Continue, 1_000, false),
            DistributedDeliveryMetadata::redelivery(2),
        )
        .unwrap();

    assert!(matches!(dispatch, CycleDispatchResult::Pending));
    let persisted = store.load_checkpoint("live-claim").unwrap().unwrap();
    assert_eq!(persisted.revision, 1);
    assert_eq!(persisted.resume_attempt, 1);
    assert_eq!(persisted.claim_token.as_deref(), Some("owner-live"));
}

#[test]
fn expired_started_unknown_tool_suspends_for_reconciliation() {
    let store = Arc::new(InMemoryCheckpointStore::new());
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
    store.create_checkpoint(checkpoint.clone()).unwrap();
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

    let CycleDispatchResult::TerminalCandidate { result, .. } = &dispatch else {
        panic!("expected terminal candidate, got {}", dispatch.kind());
    };
    assert_eq!(result.status, vv_agent::AgentStatus::ReconciliationRequired);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let persisted = store.load_checkpoint("ambiguous-tool").unwrap().unwrap();
    assert_eq!(persisted.status, CheckpointStatus::ReconciliationRequired);
    assert_eq!(persisted.resume_attempt, 2);
    assert_eq!(persisted.tool_journal[0].state, OperationState::Ambiguous);
    assert!(persisted.claim_token.is_none());
}

#[test]
fn redelivery_replays_committed_receipt_without_external_call() {
    let store = Arc::new(InMemoryCheckpointStore::new());
    let mut checkpoint = minimal_checkpoint(
        "receipt-replay",
        "task-replay",
        "run-replay",
        "trace-replay",
    );
    checkpoint
        .model_call_journal
        .push(journal_entry("model_succeeded"));
    store.create_checkpoint(checkpoint.clone()).unwrap();
    let external_calls = Arc::new(AtomicUsize::new(0));
    let executor = TestExecutor::new(move |envelope, _, progress| {
        assert_eq!(
            progress.checkpoint().model_call_journal[0].state,
            OperationState::Succeeded
        );
        let mut committed = progress.checkpoint().clone();
        committed.cycle_index = u64::from(envelope.cycle_index);
        Ok(DistributedCycleOutcome::Continue(committed))
    });
    let registry = registry_with_store(store.clone(), None);

    let dispatch = DistributedCycleWorker::new(registry)
        .with_checkpoint_executor(Arc::new(executor))
        .run_cycle_with_delivery(
            envelope(&checkpoint, 1, ClaimMode::Continue, 1_000, false),
            DistributedDeliveryMetadata::redelivery(2),
        )
        .unwrap();

    assert!(matches!(dispatch, CycleDispatchResult::Committed { .. }));
    assert_eq!(external_calls.load(Ordering::SeqCst), 0);
    let persisted = store.load_checkpoint("receipt-replay").unwrap().unwrap();
    assert_eq!(persisted.cycle_index, 1);
    assert_eq!(persisted.resume_attempt, 2);
    assert!(persisted.model_call_journal.is_empty());
}

#[test]
fn idempotent_retry_reuses_key_and_committed_cycle_absorbs_stale_redelivery() {
    let store = Arc::new(InMemoryCheckpointStore::new());
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
    store.create_checkpoint(checkpoint.clone()).unwrap();
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
        Ok(DistributedCycleOutcome::Continue(committed))
    });
    let registry = registry_with_store(store.clone(), None);
    let worker = DistributedCycleWorker::new(registry).with_checkpoint_executor(Arc::new(executor));
    let mut stale_envelope = envelope(&checkpoint, 1, ClaimMode::Continue, 1_000, false);
    stale_envelope.checkpoint_config.ambiguous_tool_policy =
        AmbiguousToolPolicy::RetryIdempotentOnly;

    let first = worker
        .run_cycle_with_delivery(
            stale_envelope.clone(),
            DistributedDeliveryMetadata::redelivery(2),
        )
        .unwrap();
    assert!(matches!(first, CycleDispatchResult::Committed { .. }));
    assert_eq!(external_calls.load(Ordering::SeqCst), 1);

    let stale_redelivery = worker
        .run_cycle_with_delivery(stale_envelope, DistributedDeliveryMetadata::redelivery(3))
        .unwrap();
    assert!(matches!(
        stale_redelivery,
        CycleDispatchResult::Committed { .. }
    ));
    assert_eq!(external_calls.load(Ordering::SeqCst), 1);
    let persisted = store.load_checkpoint("idempotent-retry").unwrap().unwrap();
    assert_eq!(persisted.cycle_index, 1);
    assert_eq!(persisted.resume_attempt, 2);
    assert!(persisted.tool_journal.is_empty());
}

#[test]
fn heartbeat_does_not_overwrite_progress_revision_or_journal() {
    let store = Arc::new(InMemoryCheckpointStore::new());
    let checkpoint = minimal_checkpoint(
        "heartbeat-progress",
        "task-heartbeat",
        "run-heartbeat",
        "trace-heartbeat",
    );
    store.create_checkpoint(checkpoint.clone()).unwrap();
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
            .load_checkpoint("heartbeat-progress")
            .map_err(|error| error.to_string())?
            .unwrap();
        assert_eq!(after_heartbeat.revision, progressed.revision);
        assert_eq!(after_heartbeat.model_call_journal.len(), 1);
        assert!(after_heartbeat.lease_expires_at_ms.unwrap() > first_expiry);
        let mut committed = after_heartbeat;
        committed.cycle_index = u64::from(envelope.cycle_index);
        Ok(DistributedCycleOutcome::Continue(committed))
    });
    let registry = registry_with_store(store.clone(), None);

    DistributedCycleWorker::new(registry)
        .with_checkpoint_executor(Arc::new(executor))
        .run_cycle(envelope(&checkpoint, 1, ClaimMode::Continue, 300, false))
        .unwrap();

    let persisted = store
        .load_checkpoint("heartbeat-progress")
        .unwrap()
        .unwrap();
    assert_eq!(persisted.revision, 3);
    assert_eq!(persisted.cycle_index, 1);
    assert!(persisted.model_call_journal.is_empty());
}

#[test]
fn terminal_candidate_retains_claim_without_finalizing_or_acknowledging() {
    let store = Arc::new(InMemoryCheckpointStore::new());
    let event_store = Arc::new(InMemoryRunEventStore::default());
    let checkpoint = minimal_checkpoint(
        "terminal-two-phase",
        "task-terminal",
        "run-terminal",
        "trace-terminal",
    );
    store.create_checkpoint(checkpoint.clone()).unwrap();
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
                json!({
                    "version": "v1",
                    "type": "run_completed",
                    "event_id": "evt-terminal-two-phase",
                    "run_id": "run-terminal",
                    "trace_id": "trace-terminal",
                    "created_at": 1.0,
                    "final_output": "done",
                    "status": "completed",
                    "completion_reason": "tool_finish",
                    "completion_tool_name": "task_finish"
                }),
            )
            .unwrap(),
        );
        Ok(DistributedCycleOutcome::Terminal(terminal))
    });
    let registry = registry_with_store(store.clone(), Some(event_store));
    let worker = DistributedCycleWorker::new(registry).with_checkpoint_executor(Arc::new(executor));
    let terminal_envelope = envelope(&checkpoint, 1, ClaimMode::Continue, 1_000, true);

    let dispatch = worker.run_cycle(terminal_envelope.clone()).unwrap();

    let CycleDispatchResult::TerminalCandidate {
        checkpoint_revision,
        ..
    } = &dispatch
    else {
        panic!("expected terminal candidate, got {}", dispatch.kind());
    };
    assert_eq!(executor_calls.load(Ordering::SeqCst), 1);
    let persisted = store
        .load_checkpoint("terminal-two-phase")
        .unwrap()
        .unwrap();
    assert_eq!(*checkpoint_revision, persisted.revision);
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
    assert!(matches!(replay, CycleDispatchResult::Pending));
    assert_eq!(executor_calls.load(Ordering::SeqCst), 1);
    assert!(store
        .load_checkpoint("terminal-two-phase")
        .unwrap()
        .is_some());
}

#[test]
fn definition_and_resume_attempt_mismatch_fail_before_claim() {
    let store = Arc::new(InMemoryCheckpointStore::new());
    let checkpoint = minimal_checkpoint(
        "identity-mismatch",
        "task-identity",
        "run-identity",
        "trace-identity",
    );
    store.create_checkpoint(checkpoint.clone()).unwrap();
    let registry = registry_with_store(store.clone(), None);
    let worker = DistributedCycleWorker::new(registry);

    let mut wrong_definition = envelope(&checkpoint, 1, ClaimMode::Continue, 1_000, false);
    wrong_definition.run_definition_digest = "d".repeat(64);
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
    wrong_attempt.resume_attempt = 2;
    assert_eq!(
        worker.run_cycle(wrong_attempt).unwrap_err(),
        "checkpoint_resume_attempt_mismatch"
    );
    let persisted = store.load_checkpoint("identity-mismatch").unwrap().unwrap();
    assert_eq!(persisted.revision, 0);
    assert!(persisted.claim_token.is_none());
}

#[test]
fn definition_validation_redacts_credentials_and_normalizes_tool_policy_sets() {
    let store = Arc::new(InMemoryCheckpointStore::new());
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
    store.create_checkpoint(checkpoint.clone()).unwrap();

    let executor = TestExecutor::new(move |envelope, _, progress| {
        let mut committed = progress.checkpoint().clone();
        committed.cycle_index = u64::from(envelope.cycle_index);
        Ok(DistributedCycleOutcome::Continue(committed))
    });
    let registry = registry_with_store(store.clone(), None);
    let worker = DistributedCycleWorker::new(registry).with_checkpoint_executor(Arc::new(executor));
    let mut envelope = envelope(&checkpoint, 1, ClaimMode::Continue, 1_000, false);
    envelope.checkpoint_config.credential_slots =
        vec!["/model/settings/extra_headers/authorization".to_string()];
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

    assert!(matches!(
        dispatch,
        CycleDispatchResult::Committed {
            committed_cycle: 1,
            ..
        }
    ));
    let persisted = store
        .load_checkpoint("normalized-definition")
        .unwrap()
        .unwrap();
    assert_eq!(persisted.cycle_index, 1);
}
