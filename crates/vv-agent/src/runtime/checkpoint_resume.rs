//! Process-local checkpoint execution controller.

use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc, Arc, Mutex,
};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::budget::BudgetUsageSnapshot;
use crate::checkpoint::{
    event_payload_digest, operation_request_digest, run_definition_digest, AcceptDeferredDecision,
    AmbiguousModelPolicy, AmbiguousToolPolicy, CheckpointConfig, CheckpointError,
    CheckpointExtension, CheckpointRenewalOutcome, CheckpointResult, CheckpointStatus, ClaimMode,
    EventCursor, OperationKind, OperationState, ReconciliationDecision, ReconciliationDecisionKind,
    ReconciliationProvider, ResumeObservation, ResumePolicy, ToolIdempotency,
    OPERATION_REQUEST_SCHEMA,
};
use crate::event_store::RunEventStore;
use crate::events::{RunEvent, RunEventPayload};
use crate::llm::{LlmError, LlmRequest};
use crate::runtime::backends::CapabilityRef;
use crate::runtime::model_calls::{
    is_definitive_model_error, model_error_code, response_usage, ModelCallCoordinator,
    ModelCallDispatchRequest, ModelCallDispatchResult, ModelCallIdentity, ModelCallTerminal,
};
use crate::runtime::state::{
    validate_extension_state_size, Checkpoint, CheckpointStore, EventOutboxEntry,
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
    pub preloaded_checkpoint: Option<Checkpoint>,
}

#[derive(Debug)]
pub(crate) enum ModelOperationOutcome {
    Response(Box<ModelCallDispatchResult>),
    Error(LlmError),
    Interrupted(Box<AgentResult>),
}

#[derive(Debug, Clone)]
pub(crate) struct ToolOperationPlan {
    pub checkpoint_key: Option<String>,
    pub operation_id: Option<String>,
    pub attempt: Option<u64>,
    pub request_digest: Option<String>,
    pub idempotency_support: ToolIdempotency,
    pub idempotency_key: Option<String>,
    pub replay_result: Option<ToolExecutionResult>,
}

struct HeartbeatHandle {
    stop: mpsc::Sender<()>,
    error: Arc<Mutex<Option<CheckpointError>>>,
    lease_expires_at_ms: Arc<AtomicU64>,
    thread: Option<JoinHandle<()>>,
}

pub(crate) struct CheckpointResumeController {
    config: CheckpointConfig,
    store: Arc<dyn CheckpointStore>,
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
    preloaded_checkpoint: Option<Checkpoint>,
    checkpoint: Option<Checkpoint>,
    created: bool,
    first_claim_is_recovery: bool,
    owned_claim_token: Option<String>,
    lease_duration_ms: u64,
    heartbeat: Option<HeartbeatHandle>,
    model_accounting: Option<ModelCallCoordinator>,
}

mod operations;
mod persistence;
mod recovery;
mod terminal_admission;

impl Drop for CheckpointResumeController {
    fn drop(&mut self) {
        self.stop_heartbeat();
    }
}

fn queue_event(checkpoint: &mut Checkpoint, event: RunEvent) -> CheckpointResult<()> {
    let event_value = serde_json::to_value(&event).map_err(|error| {
        CheckpointError::new(
            "checkpoint_event_outbox_invalid",
            format!("run event cannot be serialized: {error}"),
        )
    })?;
    let candidate = EventOutboxEntry::pending(event.event_id().as_str(), event_value)?;
    crate::runtime::state::append_event_outbox_once(&mut checkpoint.event_outbox, candidate)
}

fn raw_event_cursor(event_id: &str) -> CheckpointResult<EventCursor> {
    Ok(EventCursor::new(
        CapabilityRef::new("events.raw-sink", "1")
            .map_err(|error| CheckpointError::new("checkpoint_event_cursor_invalid", error))?,
        json!({"event_id": event_id}),
        Some(event_id.to_string()),
    ))
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

fn reconciliation_result(checkpoint: &Checkpoint, observation: ResumeObservation) -> AgentResult {
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
        resume_observations: vec![observation],
        final_answer: None,
        wait_reason: None,
        error: None,
        error_code: None,
        shared_state: checkpoint.shared_state.clone(),
        token_usage: summarize_task_token_usage(&checkpoint.model_calls),
    }
}

fn operator_abort_result(checkpoint: &Checkpoint, observation: ResumeObservation) -> AgentResult {
    let mut result = reconciliation_result(checkpoint, observation);
    result.status = AgentStatus::Failed;
    result.completion_reason = Some(crate::types::CompletionReason::Failed);
    result.error = Some(crate::types::AgentResultError::new(
        "operator_abort_with_unknown_outcome",
        "Operator accepted that the external outcome is unknown.",
        false,
    ));
    result.error_code = Some("operator_abort_with_unknown_outcome".to_string());
    result
}

pub(crate) fn apply_reconciliation_decision(
    entry: &mut OperationJournalEntry,
    decision: &ReconciliationDecision,
    checkpoint_key: &str,
    unknown_tool_observation: Option<&ResumeObservation>,
) -> CheckpointResult<()> {
    match decision.kind {
        ReconciliationDecisionKind::Retry => entry.retry()?,
        ReconciliationDecisionKind::ReplaySuccess => match entry.kind {
            OperationKind::Model => {
                entry.state = OperationState::Succeeded;
                entry.response = decision.response.clone();
                entry.result = None;
                entry.error = None;
                entry.validate()?;
            }
            OperationKind::Tool => {
                let result = reconciliation_tool_result(entry, decision)?
                    .expect("tool replay_success must carry a result");
                entry.identity_key = Some(crate::checkpoint::tool_receipt_identity_key(
                    checkpoint_key,
                    &entry.operation_id,
                    entry.attempt,
                    entry.tool_call_id.as_deref().unwrap_or_default(),
                    &entry.request_digest,
                )?);
                entry.result_digest = Some(crate::checkpoint::tool_result_digest(&result)?);
                entry.resume_observation = None;
                entry.deferred_handle = None;
                entry.state = OperationState::Succeeded;
                entry.response = None;
                entry.result = Some(result.to_dict());
                entry.error = None;
                entry.validate()?;
            }
        },
        ReconciliationDecisionKind::RecordFailure => {
            let error = decision.error.as_ref().expect("decision validated");
            if entry.kind == OperationKind::Tool {
                let synthetic_result = reconciliation_tool_result(entry, decision)?
                    .expect("tool record_failure must carry a synthetic result");
                entry.identity_key = Some(crate::checkpoint::tool_receipt_identity_key(
                    checkpoint_key,
                    &entry.operation_id,
                    entry.attempt,
                    entry.tool_call_id.as_deref().unwrap_or_default(),
                    &entry.request_digest,
                )?);
                entry.result_digest =
                    Some(crate::checkpoint::tool_result_digest(&synthetic_result)?);
                entry.resume_observation = (error.code == "tool_outcome_unknown")
                    .then(|| unknown_tool_observation.cloned())
                    .flatten();
                entry.deferred_handle = None;
                entry.result = Some(synthetic_result.to_dict());
            }
            entry.state = OperationState::Failed;
            entry.response = None;
            entry.error = Some(OperationError::new(
                &error.code,
                &error.message,
                error.retryable,
            ));
            entry.validate()?;
        }
        ReconciliationDecisionKind::AcceptDeferred => {
            let handle = decision.handle.clone().ok_or_else(|| {
                CheckpointError::new(
                    "reconciliation_decision_invalid",
                    "accept_deferred requires a handle",
                )
            })?;
            entry.deferred_handle = Some(handle);
            entry.state = OperationState::Deferred;
            entry.validate()?;
        }
        ReconciliationDecisionKind::Defer | ReconciliationDecisionKind::Abort => {}
    }
    Ok(())
}

pub(crate) fn reconciliation_tool_result(
    entry: &OperationJournalEntry,
    decision: &ReconciliationDecision,
) -> CheckpointResult<Option<ToolExecutionResult>> {
    if entry.kind != OperationKind::Tool {
        return Ok(None);
    }
    let result = match decision.kind {
        ReconciliationDecisionKind::ReplaySuccess => {
            let value = decision.result.as_ref().ok_or_else(|| {
                CheckpointError::new(
                    "reconciliation_decision_invalid",
                    "tool replay_success requires a result",
                )
            })?;
            ToolExecutionResult::from_dict(value).map_err(|error| {
                CheckpointError::new(
                    "reconciliation_decision_invalid",
                    format!("tool replay_success result is invalid: {error}"),
                )
            })?
        }
        ReconciliationDecisionKind::RecordFailure => {
            let error = decision.error.as_ref().expect("decision validated");
            let mut result = ToolExecutionResult::error(
                entry.tool_call_id.as_deref().unwrap_or_default(),
                error.message.clone(),
            )
            .with_error_code(error.code.clone());
            if error.retryable {
                result
                    .metadata
                    .insert("retryable".to_string(), Value::Bool(true));
            }
            result
        }
        _ => return Ok(None),
    };
    if result.tool_call_id != entry.tool_call_id.as_deref().unwrap_or_default() {
        return Err(CheckpointError::new(
            "reconciliation_decision_invalid",
            "tool reconciliation result does not match the journal call id",
        ));
    }
    crate::checkpoint::validate_definitive_result(&result)?;
    Ok(Some(result))
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
    let error_code = result
        .error_code
        .as_deref()
        .or_else(|| result.error.as_ref().map(|error| error.code.as_str()));
    matches!(
        error_code,
        Some(
            "operator_abort_with_unknown_outcome"
                | "cancelled_with_unknown_outcome"
                | "lease_lost_with_unknown_outcome"
        )
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    fn recovery_event(payload: RunEventPayload, created_at: f64) -> RunEvent {
        let mut event = RunEvent::new("run", "trace", "agent", Some(1), payload);
        event.created_at = created_at;
        event
            .with_event_id("evt_stable_recovery")
            .expect("stable event id")
    }

    #[test]
    fn recovery_events_reuse_existing_payload_and_reject_real_conflicts() {
        let mut checkpoint = Checkpoint::default();
        let ambiguous = recovery_event(
            RunEventPayload::OperationAmbiguous {
                checkpoint_key: "checkpoint".to_string(),
                operation_id: "operation".to_string(),
                operation_kind: OperationKind::Tool,
                risk: "unknown_tool_side_effect".to_string(),
                idempotency_support: Some(ToolIdempotency::Unknown),
            },
            1.0,
        );
        queue_event(&mut checkpoint, ambiguous.clone()).expect("first ambiguous event");
        let mut replayed = ambiguous.clone();
        replayed.created_at = 2.0;
        queue_event(&mut checkpoint, replayed).expect("ambiguous replay is idempotent");
        assert_eq!(checkpoint.event_outbox.len(), 1);
        assert_eq!(checkpoint.event_outbox[0].event["created_at"], 1.0);

        let replayed = recovery_event(
            RunEventPayload::OperationReplayed {
                checkpoint_key: "checkpoint".to_string(),
                operation_id: "operation".to_string(),
                operation_kind: OperationKind::Tool,
                receipt_state: OperationState::Succeeded,
            },
            3.0,
        );
        let mut replay_checkpoint = Checkpoint::default();
        queue_event(&mut replay_checkpoint, replayed.clone()).expect("first replay event");
        let mut replayed_retry = replayed;
        replayed_retry.created_at = 4.0;
        queue_event(&mut replay_checkpoint, replayed_retry).expect("replayed event is idempotent");
        assert_eq!(replay_checkpoint.event_outbox.len(), 1);
        assert_eq!(replay_checkpoint.event_outbox[0].event["created_at"], 3.0);

        let mut conflict = ambiguous;
        if let RunEventPayload::OperationAmbiguous { risk, .. } = &mut conflict.payload {
            *risk = "different-risk".to_string();
        }
        let error = queue_event(&mut checkpoint, conflict).expect_err("payload conflict");
        assert_eq!(error.code(), "event_identity_conflict");
    }
}
