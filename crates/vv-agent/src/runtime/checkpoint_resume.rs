//! Process-local checkpoint v2 execution controller.

use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::budget::BudgetUsageSnapshot;
use crate::checkpoint::{
    event_payload_digest, operation_request_digest, run_definition_comparison_copy,
    run_definition_digest, AmbiguousModelPolicy, AmbiguousToolPolicy, CheckpointConfig,
    CheckpointError, CheckpointExtension, CheckpointResult, CheckpointStatus, ClaimMode,
    EventCursor, OperationKind, OperationState, ReconciliationDecision, ReconciliationDecisionKind,
    ReconciliationProvider, ResumeObservation, ResumePolicy, ToolIdempotency,
    OPERATION_REQUEST_SCHEMA,
};
use crate::event_store::RunEventStore;
use crate::events::{RunEvent, RunEventPayload};
use crate::llm::{LlmError, LlmRequest};
use crate::runtime::backends::CapabilityRef;
use crate::runtime::state_v2::{
    validate_extension_state_size, CheckpointStoreV2, CheckpointV2, EventOutboxEntry,
    ExtensionStateEntry, OperationError, OperationJournalEntry,
};
use crate::runtime::token_usage::summarize_task_token_usage;
use crate::types::{
    last_assistant_output, AgentResult, AgentStatus, CycleRecord, LLMResponse, Message, Metadata,
    ToolCall, ToolExecutionResult, ToolResultStatus,
};

pub(crate) const DEFAULT_CHECKPOINT_LEASE_MS: u64 = 5 * 60 * 1_000;

pub(crate) type CheckpointController = Arc<Mutex<CheckpointResumeController>>;
pub(crate) type CheckpointEventSink =
    Arc<dyn Fn(RunEvent) -> Result<(), String> + Send + Sync + 'static>;

pub(crate) struct CheckpointControllerRequest {
    pub config: CheckpointConfig,
    pub task_id: String,
    pub run_id: String,
    pub trace_id: String,
    pub agent_name: String,
    pub run_definition: Value,
    pub run_definition_digest: String,
    pub initial_messages: Vec<Message>,
    pub initial_shared_state: Metadata,
    pub initial_budget_usage: Option<BudgetUsageSnapshot>,
    pub extensions: Vec<Arc<dyn CheckpointExtension>>,
    pub reconciliation_provider: Option<Arc<dyn ReconciliationProvider>>,
    pub event_sink: CheckpointEventSink,
    pub event_store: Option<Arc<dyn RunEventStore>>,
    pub preloaded_checkpoint: Option<CheckpointV2>,
}

#[derive(Debug)]
pub(crate) enum ModelOperationOutcome {
    Response(Box<LLMResponse>),
    Error(LlmError),
    Interrupted(Box<AgentResult>),
}

#[derive(Debug, Clone)]
pub(crate) struct ToolOperationPlan {
    pub idempotency_key: String,
    pub replay_result: Option<ToolExecutionResult>,
}

struct HeartbeatHandle {
    stop: mpsc::Sender<()>,
    error: Arc<Mutex<Option<CheckpointError>>>,
    thread: Option<JoinHandle<()>>,
}

pub(crate) struct CheckpointResumeController {
    config: CheckpointConfig,
    store: Arc<dyn CheckpointStoreV2>,
    task_id: String,
    run_id: String,
    trace_id: String,
    agent_name: String,
    run_definition: Value,
    run_definition_digest: String,
    initial_messages: Vec<Message>,
    initial_shared_state: Metadata,
    initial_budget_usage: Option<BudgetUsageSnapshot>,
    extensions: BTreeMap<String, Arc<dyn CheckpointExtension>>,
    reconciliation_provider: Option<Arc<dyn ReconciliationProvider>>,
    event_sink: CheckpointEventSink,
    event_store: Option<Arc<dyn RunEventStore>>,
    preloaded_checkpoint: Option<CheckpointV2>,
    checkpoint: Option<CheckpointV2>,
    created: bool,
    first_claim_is_recovery: bool,
    owned_claim_token: Option<String>,
    lease_duration_ms: u64,
    heartbeat: Option<HeartbeatHandle>,
}

mod operations;
mod persistence;
mod recovery;

impl Drop for CheckpointResumeController {
    fn drop(&mut self) {
        self.stop_heartbeat();
    }
}

fn queue_event(checkpoint: &mut CheckpointV2, event: RunEvent) -> CheckpointResult<()> {
    let event_value = serde_json::to_value(&event).map_err(|error| {
        CheckpointError::new(
            "checkpoint_event_outbox_invalid",
            format!("run event cannot be serialized: {error}"),
        )
    })?;
    let event_id = event.event_id().as_str().to_string();
    let candidate = EventOutboxEntry::pending(event_id.clone(), event_value)?;
    if let Some(existing) = checkpoint
        .event_outbox
        .iter()
        .find(|entry| entry.event_id == event_id)
    {
        existing.verify_payload()?;
        if existing.payload_digest != candidate.payload_digest {
            return Err(CheckpointError::new(
                "event_identity_conflict",
                format!("checkpoint event id {event_id:?} has conflicting payload bytes"),
            ));
        }
        return Ok(());
    }
    checkpoint.event_outbox.push(candidate);
    Ok(())
}

fn raw_event_cursor(event_id: &str) -> CheckpointResult<EventCursor> {
    Ok(EventCursor::new(
        CapabilityRef::new("events.raw-sink", "1")
            .map_err(|error| CheckpointError::new("checkpoint_event_cursor_invalid", error))?,
        json!({"event_id": event_id}),
        Some(event_id.to_string()),
    ))
}

fn model_operation_id(cycle_index: u32, operation_slot: &str) -> String {
    let digest = Sha256::digest(operation_slot.as_bytes());
    format!(
        "op_model_cycle_{}_{}",
        cycle_index,
        &format!("{digest:x}")[..16]
    )
}

fn tool_idempotency_key(checkpoint_key: &str, cycle_index: u32, call_id: &str) -> String {
    let source = format!("{checkpoint_key}\0{cycle_index}\0{call_id}");
    let digest = Sha256::digest(source.as_bytes());
    format!("idem_{}", &format!("{digest:x}")[..32])
}

fn stable_event_id_for(checkpoint_key: &str, event_type: &str, coordinates: &[&str]) -> String {
    let mut source = format!("{checkpoint_key}\0{event_type}");
    for coordinate in coordinates {
        source.push('\0');
        source.push_str(coordinate);
    }
    let digest = Sha256::digest(source.as_bytes());
    format!("evt_{}", &format!("{digest:x}")[..32])
}

fn event_type(event: &RunEvent) -> &str {
    match event.payload() {
        RunEventPayload::RunCompleted { .. } => "run_completed",
        RunEventPayload::RunFailed { .. } => "run_failed",
        RunEventPayload::RunCancelled { .. } => "run_cancelled",
        _ => "terminal",
    }
}

fn observation(entry: &OperationJournalEntry) -> ResumeObservation {
    ResumeObservation {
        operation_id: entry.operation_id.clone(),
        operation_kind: entry.kind,
        cycle_index: entry.cycle_index,
        state: OperationState::Ambiguous,
        risk: match entry.kind {
            OperationKind::Model => "duplicate_model_request_and_cost".to_string(),
            OperationKind::Tool => "unknown_tool_side_effect".to_string(),
        },
        idempotency_support: entry.idempotency_support,
    }
}

fn reconciliation_result(checkpoint: &CheckpointV2, observation: ResumeObservation) -> AgentResult {
    AgentResult {
        status: AgentStatus::ReconciliationRequired,
        messages: checkpoint.messages.clone(),
        cycles: checkpoint.cycles.clone(),
        completion_reason: None,
        completion_tool_name: None,
        partial_output: last_assistant_output(&checkpoint.cycles),
        budget_usage: checkpoint.budget_usage.clone(),
        budget_exhaustion: None,
        checkpoint_key: Some(checkpoint.checkpoint_key.clone()),
        resume_observation: Some(observation),
        final_answer: None,
        wait_reason: None,
        error: None,
        shared_state: checkpoint.shared_state.clone(),
        token_usage: summarize_task_token_usage(&checkpoint.cycles),
    }
}

fn operator_abort_result(checkpoint: &CheckpointV2, observation: ResumeObservation) -> AgentResult {
    let mut result = reconciliation_result(checkpoint, observation);
    result.status = AgentStatus::Failed;
    result.completion_reason = Some(crate::types::CompletionReason::Failed);
    result.error = Some("operator_abort_with_unknown_outcome".to_string());
    result
}

fn apply_reconciliation_decision(
    entry: &mut OperationJournalEntry,
    decision: &ReconciliationDecision,
) -> CheckpointResult<()> {
    match decision.kind {
        ReconciliationDecisionKind::Retry => entry.retry()?,
        ReconciliationDecisionKind::ReplaySuccess => {
            entry.state = OperationState::Succeeded;
            entry.response = decision.response.clone();
            entry.result = decision.result.clone();
            entry.error = None;
            entry.validate()?;
        }
        ReconciliationDecisionKind::RecordFailure => {
            let error = decision.error.as_ref().expect("decision validated");
            entry.state = OperationState::Failed;
            entry.response = None;
            entry.result = None;
            entry.error = Some(OperationError::new(
                &error.code,
                &error.message,
                error.retryable,
            ));
            entry.validate()?;
        }
        ReconciliationDecisionKind::Defer | ReconciliationDecisionKind::Abort => {}
    }
    Ok(())
}

fn definitive_model_error(error: &LlmError) -> bool {
    if matches!(
        error,
        LlmError::ScriptExhausted | LlmError::CompactionExhausted(_)
    ) {
        return true;
    }
    let message = error.to_string().to_ascii_lowercase();
    [
        "context length",
        "context_length_exceeded",
        "maximum context length",
        "prompt is too long",
        "request too large",
    ]
    .iter()
    .any(|marker| message.contains(marker))
}

fn checkpoint_status(status: AgentStatus) -> CheckpointResult<CheckpointStatus> {
    match status {
        AgentStatus::WaitUser => Ok(CheckpointStatus::WaitUser),
        AgentStatus::Completed => Ok(CheckpointStatus::Completed),
        AgentStatus::Failed => Ok(CheckpointStatus::Failed),
        AgentStatus::MaxCycles => Ok(CheckpointStatus::MaxCycles),
        _ => Err(CheckpointError::new(
            "checkpoint_status_invalid",
            "terminal finalization requires a terminal AgentStatus",
        )),
    }
}

fn is_operator_abort(result: &AgentResult) -> bool {
    result.status == AgentStatus::Failed
        && result.error.as_deref() == Some("operator_abort_with_unknown_outcome")
        && result.resume_observation.is_some()
}

fn now_ms() -> CheckpointResult<u64> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| CheckpointError::new("checkpoint_clock_invalid", error.to_string()))?
        .as_millis();
    u64::try_from(millis).map_err(|_| {
        CheckpointError::new(
            "checkpoint_clock_invalid",
            "system time is outside the checkpoint integer range",
        )
    })
}

#[allow(dead_code)]
fn verify_event_digest(entry: &EventOutboxEntry) -> CheckpointResult<()> {
    if event_payload_digest(&entry.event)? != entry.payload_digest {
        return Err(CheckpointError::new(
            "event_identity_conflict",
            "event outbox payload digest mismatch",
        ));
    }
    Ok(())
}
