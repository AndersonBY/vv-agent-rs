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
    ControllerCommand, ControllerCommandVariant, ControllerHandle, CycleDispatchResult,
    EventOutboxEntry, ExtensionStateEntry, HostInteractionAdmissionContext, HostInteractionMessage,
    HostInteractionRequest, InMemoryCheckpointStore, InMemoryRunEventStore, LLMResponse, Message,
    ModelCallRecord, ModelCallStatus, ModelSettings, OperationJournalEntry, OperationState,
    PromptBundle, ReconciliationDecision, ReconciliationError, ReconciliationProvider,
    ResumePolicy, RunBudgetLimits, RunEvent, RunEventPayload, RunEventReplayQuery, RunEventStore,
    RuntimeRecipe, ScriptedLlmClient, TokenUsage, ToolArtifactRef, ToolExecutionResult,
    ToolIdempotency,
};

const ENVELOPE_FIXTURE: &str = include_str!("fixtures/parity/distributed_run_envelope.json");
const CODEC_FIXTURE: &str = include_str!("fixtures/parity/checkpoint_codec.json");
const JOURNAL_FIXTURE: &str = include_str!("fixtures/parity/operation_journal.json");

#[path = "distributed_checkpoint/abort.rs"]
mod distributed_checkpoint_abort;
#[path = "distributed_checkpoint/outbox.rs"]
mod distributed_checkpoint_outbox;
#[path = "distributed_checkpoint/receipt_retry.rs"]
mod distributed_checkpoint_receipt_retry;
#[path = "distributed_checkpoint/reconciliation.rs"]
mod distributed_checkpoint_reconciliation;
#[path = "distributed_checkpoint/terminal_replay.rs"]
mod distributed_checkpoint_terminal_replay;

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

fn attach_succeeded_model_accounting(
    checkpoint: &mut vv_agent::Checkpoint,
    entry: &OperationJournalEntry,
) {
    let cycle_index = u32::try_from(entry.cycle_index).expect("model cycle index");
    let attempt = u32::try_from(entry.attempt).expect("model attempt");
    let call_id = entry.call_id.clone().expect("model call id");
    let operation = entry.model_operation.expect("model operation");
    let backend = entry.backend.clone().expect("model backend");
    let model = entry.model.clone().expect("model name");
    let usage: TokenUsage = serde_json::from_value(
        entry.response.as_ref().expect("model response")["token_usage"].clone(),
    )
    .expect("model token usage");

    checkpoint.model_calls.push(ModelCallRecord {
        call_id: call_id.clone(),
        operation_id: entry.operation_id.clone(),
        attempt,
        operation,
        cycle_index,
        backend: backend.clone(),
        model: model.clone(),
        status: ModelCallStatus::Completed,
        usage: usage.clone(),
        error_code: None,
    });
    let started = RunEvent::model_call_started(
        &checkpoint.root_run_id,
        &checkpoint.trace_id,
        &checkpoint.task_id,
        cycle_index,
        &call_id,
        &entry.operation_id,
        attempt,
        operation,
        &backend,
        &model,
    );
    let completed = RunEvent::model_call_completed(
        &checkpoint.root_run_id,
        &checkpoint.trace_id,
        &checkpoint.task_id,
        cycle_index,
        &call_id,
        &entry.operation_id,
        attempt,
        operation,
        &backend,
        &model,
        usage,
    );
    for event in [started, completed] {
        let event_id = event.event_id().as_str().to_string();
        checkpoint.event_outbox.push(
            EventOutboxEntry::pending(
                event_id,
                serde_json::to_value(event).expect("model accounting event"),
            )
            .expect("model accounting outbox entry"),
        );
    }
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
        PromptBundle::from_instruction_text("You are a careful assistant.")
            .expect("valid prompt bundle"),
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
    task.metadata.insert(
        "session_memory_enabled".to_string(),
        checkpoint.run_definition["runtime_controls"]["session_memory_enabled"].clone(),
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

#[test]
fn distributed_envelope_round_trips_message_artifact_ref() {
    let artifact_ref = ToolArtifactRef {
        path: ".vv-agent/artifacts/distributed/call.txt".to_string(),
        media_type: "text/plain".to_string(),
        encoding: "utf-8".to_string(),
        size_bytes: 17,
        sha256: "c".repeat(64),
    };
    let mut message = Message::tool("bounded preview", "call");
    message.artifact_ref = Some(artifact_ref.clone());
    let mut task = AgentTask::new(
        "distributed-artifact",
        "test-model",
        PromptBundle::from_instruction_text("system").expect("prompt bundle"),
        "run",
    );
    task.initial_messages.push(message);
    let mut recipe = RuntimeRecipe::new("settings.json", "test", "test-model", ".");
    recipe.capabilities.checkpoint_store_ref = Some(store_ref());
    let envelope = DistributedRunEnvelope::for_cycle(
        task,
        recipe,
        1,
        DEFAULT_CYCLE_NAME,
        Some("run-artifact".to_string()),
        None,
        1_000,
        None,
        "run-artifact",
        "trace-artifact",
        "d".repeat(64),
        ClaimMode::Continue,
        1,
        DistributedCheckpointConfig {
            key: "checkpoint-artifact".to_string(),
            resume_policy: ResumePolicy::RequireExisting,
            ambiguous_model_policy: AmbiguousModelPolicy::RequireReconciliation,
            ambiguous_tool_policy: AmbiguousToolPolicy::RequireReconciliation,
            required_extension_namespaces: Vec::new(),
            max_extension_state_bytes: 262_144,
            credential_slots: Vec::new(),
        },
    )
    .expect("distributed envelope");

    let wire = envelope.to_dict();
    let restored = DistributedRunEnvelope::from_dict(&wire).expect("restored envelope");

    assert_eq!(
        restored.task.initial_messages[0].artifact_ref,
        Some(artifact_ref)
    );
}

fn set_path(payload: &mut Value, path: &[Value], value: Value) {
    let mut target = payload;
    for key in &path[..path.len() - 1] {
        target = &mut target[key.as_str().expect("path key")];
    }
    target[path.last().and_then(Value::as_str).expect("final path key")] = value;
}

fn remove_path(payload: &mut Value, path: &[Value]) {
    let mut target = payload;
    for key in &path[..path.len() - 1] {
        target = &mut target[key.as_str().expect("path key")];
    }
    target
        .as_object_mut()
        .expect("path parent object")
        .remove(path.last().and_then(Value::as_str).expect("final path key"));
}

#[test]
fn distributed_envelope_accepts_only_the_current_wire_shape() {
    let contract = fixture(ENVELOPE_FIXTURE);
    let canonical = contract["canonical_envelope"].clone();
    let envelope = DistributedRunEnvelope::from_dict(&canonical).unwrap();
    assert_eq!(envelope.to_dict(), canonical);
    assert_eq!(serde_json::to_value(&envelope).unwrap(), canonical);
    assert!(canonical["task"].get("prompt_bundle").is_some());
    assert!(canonical["task"].get("system_prompt").is_none());

    let mut legacy_task = canonical.clone();
    legacy_task["task"]["system_prompt"] = json!("legacy prompt");
    assert!(DistributedRunEnvelope::from_dict(&legacy_task)
        .unwrap_err()
        .contains("unknown field `system_prompt`"));

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
        let path = case["path"].as_array().unwrap();
        if case["operation"] == "remove" {
            remove_path(&mut payload, path);
        } else {
            set_path(&mut payload, path, case["value"].clone());
        }
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
fn worker_claim_is_blocked_until_host_response_recovery() {
    let store = Arc::new(InMemoryCheckpointStore::new());
    let checkpoint = minimal_checkpoint(
        "worker-host-recovery-barrier",
        "task-host-recovery-barrier",
        "run-host-recovery-barrier",
        "trace-host-recovery-barrier",
    );
    let key = checkpoint.checkpoint_key.clone();
    store
        .create_checkpoint(initial_checkpoint(checkpoint))
        .expect("create initial checkpoint");
    let claimed = store
        .claim_checkpoint(&key, 1, "worker-owner", 10_000, 0, ClaimMode::Continue)
        .expect("claim checkpoint")
        .expect("claimed checkpoint");
    let request = HostInteractionRequest::new(
        "interaction-worker-host-recovery",
        1,
        "operation-worker-host-recovery",
        "tool-worker-host-recovery",
        "Choose an option.",
    )
    .expect("host interaction request");
    let admission =
        HostInteractionAdmissionContext::new(&key, claimed.revision, "worker-owner", 1, 0, 10_000)
            .expect("host interaction admission");
    let admitted = store
        .produce_host_interaction(request.clone(), &admission)
        .expect("produce host interaction");
    let command = ControllerCommand::new(
        "command-worker-host-recovery",
        ControllerHandle::new(&key, &claimed.root_run_id, &claimed.trace_id)
            .expect("controller handle"),
        claimed.resume_attempt,
        admitted.checkpoint_revision,
        ControllerCommandVariant::HostInteractionResponse {
            interaction_id: request.interaction_id.clone(),
            logical_cycle: request.logical_cycle,
            operation_id: request.operation_id.clone(),
            tool_call_id: request.tool_call_id.clone(),
            request_digest: request.request_digest.clone(),
            response: HostInteractionMessage::user("approved").expect("host response"),
        },
    )
    .expect("host response command");
    store
        .resolve_controller_command(command)
        .expect("resolve host response");
    let before = store
        .load_checkpoint(&key)
        .expect("load barrier checkpoint")
        .expect("barrier checkpoint");
    let executor = TestExecutor::new(|envelope, _, progress| {
        let mut committed = progress.checkpoint().clone();
        committed.cycle_index = u64::from(envelope.cycle_index);
        Ok(DistributedCycleOutcome::Continue(committed))
    });
    let registry = registry_with_store(store.clone(), None);
    let worker = DistributedCycleWorker::new(registry).with_checkpoint_executor(Arc::new(executor));
    for claim_mode in [ClaimMode::Continue, ClaimMode::Recovery] {
        let error = worker
            .run_cycle(envelope(&before, 1, claim_mode, 1_000, false))
            .expect_err("ordinary worker claim must stop at host recovery barrier");
        assert!(
            error.contains("host_interaction_recovery_required"),
            "unexpected worker barrier error: {error}"
        );
        let after = store
            .load_checkpoint(&key)
            .expect("load after worker barrier")
            .expect("checkpoint after worker barrier");
        assert_eq!(after, before);
    }
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
    let prompt_bundle = PromptBundle::from_value(&checkpoint.run_definition["prompt_bundle"])
        .expect("frozen prompt bundle");
    checkpoint.messages = vec![vv_agent::Message::system(prompt_bundle.flatten())];
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
    let checkpoint = minimal_checkpoint("live-claim", "task-live", "run-live", "trace-live");
    let now_ms = u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap();
    let lease_expires_at_ms = now_ms + 60_000;
    store
        .create_checkpoint(initial_checkpoint(checkpoint))
        .unwrap();
    let checkpoint = store
        .claim_checkpoint(
            "live-claim",
            1,
            "owner-live",
            lease_expires_at_ms,
            now_ms,
            ClaimMode::Continue,
        )
        .unwrap()
        .unwrap();
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
    let checkpoint = create_claimed_snapshot(store.as_ref(), checkpoint, "expired-owner", 1, 0);
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
    let succeeded = journal_entry("model_succeeded");
    attach_succeeded_model_accounting(&mut checkpoint, &succeeded);
    checkpoint.model_call_journal.push(succeeded);
    let mut checkpoint =
        create_claimed_snapshot(store.as_ref(), checkpoint, "replay-owner", 10_000, 1);
    checkpoint.claim_token = None;
    checkpoint.claimed_cycle = None;
    checkpoint.lease_expires_at_ms = None;
    store.save_checkpoint(checkpoint.clone()).unwrap();
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
        let completed = journal_entry("model_succeeded");
        attach_succeeded_model_accounting(&mut planned, &completed);
        planned.model_call_journal.push(completed);
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
    assert_eq!(persisted.revision, 6);
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
                    "version": "v5",
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
