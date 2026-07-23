use std::collections::BTreeMap;
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex};

use crate::budget::{BudgetExhaustion, BudgetUsageSnapshot};
use crate::events::{ModelCallFailureOutcome, RunEvent, RunEventPayload};
use crate::llm::{LlmError, LlmRequest};
use crate::types::{
    LLMResponse, ModelCallOperation, ModelCallRecord, ModelCallStatus, TaskTokenUsage, TokenUsage,
};

use super::token_usage::{normalize_token_usage, summarize_task_token_usage};

const DEFINITIVE_ERROR_MARKERS: &[&str] = &[
    "context length",
    "context_length_exceeded",
    "maximum context length",
    "prompt is too long",
    "prompt_too_long",
    "request too large",
    "too many tokens",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelCallIdentity {
    pub call_id: String,
    pub operation_id: String,
    pub attempt: u32,
    pub operation: ModelCallOperation,
    pub cycle_index: u32,
    pub backend: String,
    pub model: String,
}

impl ModelCallIdentity {
    pub(crate) fn create(
        operation_id: impl Into<String>,
        attempt: u32,
        operation: ModelCallOperation,
        cycle_index: u32,
        backend: impl Into<String>,
        model: impl Into<String>,
    ) -> Result<Self, String> {
        let operation_id = operation_id.into();
        let backend = backend.into();
        let model = model.into();
        for (field, value) in [
            ("operation_id", operation_id.as_str()),
            ("backend", backend.as_str()),
            ("model", model.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(format!("{field} must be a non-empty string"));
            }
        }
        if attempt == 0 {
            return Err("attempt must be positive".to_string());
        }
        if cycle_index == 0 {
            return Err("cycle_index must be positive".to_string());
        }
        Ok(Self {
            call_id: format!("{operation_id}:attempt:{attempt}"),
            operation_id,
            attempt,
            operation,
            cycle_index,
            backend,
            model,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ModelCallLedger {
    records: Arc<Mutex<Vec<ModelCallRecord>>>,
}

impl ModelCallLedger {
    pub(crate) fn replace(&self, records: Vec<ModelCallRecord>) -> Result<(), String> {
        let usage = summarize_task_token_usage_checked(&records)?;
        *self
            .records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = usage.model_calls;
        Ok(())
    }

    pub(crate) fn append(&self, record: ModelCallRecord) -> Result<(), String> {
        let mut records = self
            .records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if records
            .iter()
            .any(|existing| existing.call_id == record.call_id)
        {
            return Err("model_call_id_duplicate".to_string());
        }
        records.push(record);
        Ok(())
    }

    pub(crate) fn records(&self) -> Vec<ModelCallRecord> {
        self.records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn usage(&self) -> TaskTokenUsage {
        summarize_task_token_usage(&self.records())
    }

    pub(crate) fn previous_agent_input_tokens(&self, cycle_index: u32) -> Option<u64> {
        self.records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .rev()
            .find(|record| {
                record.operation == ModelCallOperation::AgentCycle
                    && record.cycle_index < cycle_index
                    && record.status == ModelCallStatus::Completed
            })
            .and_then(|record| record.usage.input_tokens)
    }
}

fn summarize_task_token_usage_checked(
    records: &[ModelCallRecord],
) -> Result<TaskTokenUsage, String> {
    let mut usage = TaskTokenUsage::default();
    for record in records {
        usage.add_model_call(record.clone())?;
    }
    Ok(usage)
}

#[derive(Debug, Clone)]
pub(crate) struct ModelCallDispatchResult {
    pub response: LLMResponse,
    pub usage: TokenUsage,
    pub budget_exhaustion: Option<BudgetExhaustion>,
}

#[derive(Clone, Copy)]
pub(crate) struct ModelCallDispatchRequest<'a> {
    pub cycle_index: u32,
    pub operation_slot: &'a str,
    pub operation: ModelCallOperation,
    pub backend: &'a str,
    pub model: &'a str,
    pub request: &'a LlmRequest,
    pub accounting: &'a ModelCallCoordinator,
}

type ModelEventSink = Arc<dyn Fn(&RunEvent) + Send + Sync + 'static>;
pub(crate) type ModelBudgetObserver =
    Arc<dyn Fn(u32, &TokenUsage) -> ModelCallBudgetUpdate + Send + Sync + 'static>;

#[derive(Debug, Clone, Default)]
pub(crate) struct ModelCallBudgetUpdate {
    pub exhaustion: Option<BudgetExhaustion>,
    pub snapshot: Option<BudgetUsageSnapshot>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ModelCallBudgetObservation {
    pub exhaustion: Option<BudgetExhaustion>,
    pub event: Option<RunEvent>,
}

#[derive(Debug, Clone)]
pub(crate) struct ModelCallTerminal {
    pub record: ModelCallRecord,
    pub event: RunEvent,
    pub budget: ModelCallBudgetObservation,
}

#[derive(Clone)]
pub(crate) struct ModelCallCoordinator {
    pub ledger: ModelCallLedger,
    run_id: String,
    trace_id: String,
    agent_name: String,
    session_id: Option<String>,
    parent_run_id: Option<String>,
    event_sink: Option<ModelEventSink>,
    budget_observer: Option<ModelBudgetObserver>,
    slot_counts: Arc<Mutex<BTreeMap<(u32, String), u32>>>,
}

impl std::fmt::Debug for ModelCallCoordinator {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ModelCallCoordinator")
            .field("run_id", &self.run_id)
            .field("trace_id", &self.trace_id)
            .field("agent_name", &self.agent_name)
            .field("session_id", &self.session_id)
            .field("parent_run_id", &self.parent_run_id)
            .field("model_call_count", &self.ledger.records().len())
            .finish()
    }
}

impl ModelCallCoordinator {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        ledger: ModelCallLedger,
        run_id: impl Into<String>,
        trace_id: impl Into<String>,
        agent_name: impl Into<String>,
        session_id: Option<String>,
        parent_run_id: Option<String>,
        event_sink: Option<ModelEventSink>,
        budget_observer: Option<ModelBudgetObserver>,
    ) -> Self {
        Self {
            ledger,
            run_id: run_id.into(),
            trace_id: trace_id.into(),
            agent_name: agent_name.into(),
            session_id,
            parent_run_id,
            event_sink,
            budget_observer,
            slot_counts: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn dispatch<F>(
        &self,
        operation: ModelCallOperation,
        cycle_index: u32,
        operation_slot: &str,
        backend: &str,
        model: &str,
        _request: &LlmRequest,
        invoke: F,
    ) -> Result<ModelCallDispatchResult, LlmError>
    where
        F: FnOnce() -> Result<LLMResponse, LlmError>,
    {
        let identity = self
            .new_identity(
                cycle_index,
                operation_slot,
                operation,
                backend,
                model,
                1,
                None,
            )
            .map_err(LlmError::Request)?;
        self.emit(self.started_event(&identity));
        let outcome = catch_unwind(AssertUnwindSafe(invoke));
        let response = match outcome {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                let ambiguous = !is_definitive_model_error(&error);
                let terminal =
                    self.failed_terminal(&identity, model_error_code(&error), ambiguous, None);
                self.commit_terminal(terminal).map_err(LlmError::Request)?;
                return Err(error);
            }
            Err(panic) => {
                let terminal = self.failed_terminal(
                    &identity,
                    "model_request_panicked".to_string(),
                    true,
                    None,
                );
                let _ = self.commit_terminal(terminal);
                resume_unwind(panic);
            }
        };
        let usage = response_usage(&response);
        let terminal = self.completed_terminal(&identity, usage.clone());
        let budget_exhaustion = terminal.budget.exhaustion.clone();
        self.commit_terminal(terminal).map_err(LlmError::Request)?;
        Ok(ModelCallDispatchResult {
            response,
            usage,
            budget_exhaustion,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_identity(
        &self,
        cycle_index: u32,
        operation_slot: &str,
        operation: ModelCallOperation,
        backend: &str,
        model: &str,
        attempt: u32,
        operation_id: Option<String>,
    ) -> Result<ModelCallIdentity, String> {
        let operation_id = match operation_id {
            Some(operation_id) => operation_id,
            None => {
                let normalized_slot = normalize_operation_slot(operation_slot)?;
                let mut counts = self
                    .slot_counts
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let count = counts
                    .entry((cycle_index, normalized_slot.clone()))
                    .and_modify(|count| *count += 1)
                    .or_insert(1);
                let slot = if *count == 1 {
                    normalized_slot
                } else {
                    format!("{normalized_slot}_{count}")
                };
                format!("op_model_cycle_{cycle_index}_{slot}")
            }
        };
        ModelCallIdentity::create(
            operation_id,
            attempt,
            operation,
            cycle_index,
            backend,
            model,
        )
    }

    pub(crate) fn started_event(&self, identity: &ModelCallIdentity) -> RunEvent {
        self.decorate_event(RunEvent::model_call_started(
            &self.run_id,
            &self.trace_id,
            &self.agent_name,
            identity.cycle_index,
            &identity.call_id,
            &identity.operation_id,
            identity.attempt,
            identity.operation,
            &identity.backend,
            &identity.model,
        ))
    }

    pub(crate) fn completed_event(
        &self,
        identity: &ModelCallIdentity,
        usage: TokenUsage,
    ) -> RunEvent {
        self.decorate_event(RunEvent::model_call_completed(
            &self.run_id,
            &self.trace_id,
            &self.agent_name,
            identity.cycle_index,
            &identity.call_id,
            &identity.operation_id,
            identity.attempt,
            identity.operation,
            &identity.backend,
            &identity.model,
            usage,
        ))
    }

    pub(crate) fn failed_event(
        &self,
        identity: &ModelCallIdentity,
        error_code: &str,
        ambiguous: bool,
        usage: TokenUsage,
    ) -> RunEvent {
        self.decorate_event(RunEvent::model_call_failed(
            &self.run_id,
            &self.trace_id,
            &self.agent_name,
            identity.cycle_index,
            &identity.call_id,
            &identity.operation_id,
            identity.attempt,
            identity.operation,
            &identity.backend,
            &identity.model,
            if ambiguous {
                ModelCallFailureOutcome::Ambiguous
            } else {
                ModelCallFailureOutcome::Definitive
            },
            usage,
            error_code,
        ))
    }

    pub(crate) fn completed_record(
        &self,
        identity: &ModelCallIdentity,
        usage: TokenUsage,
    ) -> ModelCallRecord {
        ModelCallRecord {
            call_id: identity.call_id.clone(),
            operation_id: identity.operation_id.clone(),
            attempt: identity.attempt,
            operation: identity.operation,
            cycle_index: identity.cycle_index,
            backend: identity.backend.clone(),
            model: identity.model.clone(),
            status: ModelCallStatus::Completed,
            usage,
            error_code: None,
        }
    }

    pub(crate) fn completed_terminal(
        &self,
        identity: &ModelCallIdentity,
        usage: TokenUsage,
    ) -> ModelCallTerminal {
        ModelCallTerminal {
            record: self.completed_record(identity, usage.clone()),
            event: self.completed_event(identity, usage.clone()),
            budget: self.observe_budget(identity, &usage),
        }
    }

    pub(crate) fn failed_record(
        &self,
        identity: &ModelCallIdentity,
        error_code: String,
        ambiguous: bool,
        usage: TokenUsage,
    ) -> ModelCallRecord {
        ModelCallRecord {
            call_id: identity.call_id.clone(),
            operation_id: identity.operation_id.clone(),
            attempt: identity.attempt,
            operation: identity.operation,
            cycle_index: identity.cycle_index,
            backend: identity.backend.clone(),
            model: identity.model.clone(),
            status: if ambiguous {
                ModelCallStatus::Ambiguous
            } else {
                ModelCallStatus::Failed
            },
            usage,
            error_code: Some(error_code),
        }
    }

    pub(crate) fn failed_terminal(
        &self,
        identity: &ModelCallIdentity,
        error_code: String,
        ambiguous: bool,
        usage: Option<TokenUsage>,
    ) -> ModelCallTerminal {
        let usage = usage.unwrap_or_default();
        ModelCallTerminal {
            record: self.failed_record(identity, error_code.clone(), ambiguous, usage.clone()),
            event: self.failed_event(identity, &error_code, ambiguous, usage.clone()),
            budget: self.observe_budget(identity, &usage),
        }
    }

    pub(crate) fn commit_terminal(&self, terminal: ModelCallTerminal) -> Result<(), String> {
        self.ledger.append(terminal.record)?;
        self.emit(terminal.event);
        if let Some(event) = terminal.budget.event {
            self.emit(event);
        }
        Ok(())
    }

    fn observe_budget(
        &self,
        identity: &ModelCallIdentity,
        usage: &TokenUsage,
    ) -> ModelCallBudgetObservation {
        let update = self
            .budget_observer
            .as_ref()
            .map(|observer| observer(identity.cycle_index, usage))
            .unwrap_or_default();
        let event = update.snapshot.map(|budget_usage| {
            let payload = match update.exhaustion.as_ref() {
                Some(budget_exhaustion) => RunEventPayload::BudgetExhausted {
                    enforcement_boundary: budget_exhaustion.enforcement_boundary,
                    budget_usage,
                    budget_exhaustion: budget_exhaustion.clone(),
                },
                None => RunEventPayload::BudgetSnapshot {
                    enforcement_boundary:
                        crate::budget::BudgetEnforcementBoundary::ModelCallComplete,
                    budget_usage,
                },
            };
            self.decorate_event(RunEvent::new(
                &self.run_id,
                &self.trace_id,
                &self.agent_name,
                Some(identity.cycle_index),
                payload,
            ))
        });
        ModelCallBudgetObservation {
            exhaustion: update.exhaustion,
            event,
        }
    }

    pub(crate) fn emit(&self, event: RunEvent) {
        if let Some(sink) = &self.event_sink {
            sink(&event);
        }
    }

    fn decorate_event(&self, mut event: RunEvent) -> RunEvent {
        if let Some(session_id) = &self.session_id {
            event = event.with_session_id(session_id);
        }
        if let Some(parent_run_id) = &self.parent_run_id {
            event = event.with_parent_run_id(parent_run_id);
        }
        event
    }
}

pub(crate) fn response_usage(response: &LLMResponse) -> TokenUsage {
    if response.token_usage.has_usage() {
        response.token_usage.clone()
    } else {
        normalize_token_usage(
            response
                .raw
                .get("usage")
                .unwrap_or(&serde_json::Value::Null),
        )
    }
}

pub(crate) fn is_definitive_model_error(error: &LlmError) -> bool {
    if matches!(
        error,
        LlmError::ScriptExhausted | LlmError::CompactionExhausted(_)
    ) {
        return true;
    }
    let text = error.to_string().to_ascii_lowercase();
    DEFINITIVE_ERROR_MARKERS
        .iter()
        .any(|marker| text.contains(marker))
}

pub(crate) fn model_error_code(error: &LlmError) -> String {
    if is_definitive_model_error(error)
        && DEFINITIVE_ERROR_MARKERS
            .iter()
            .any(|marker| error.to_string().to_ascii_lowercase().contains(marker))
    {
        "prompt_too_long".to_string()
    } else {
        "model_request_failed".to_string()
    }
}

fn normalize_operation_slot(value: &str) -> Result<String, String> {
    let mut normalized = String::new();
    let mut separator_pending = false;
    for character in value.trim().to_ascii_lowercase().chars() {
        if character.is_ascii_alphanumeric() {
            if separator_pending && !normalized.is_empty() {
                normalized.push('_');
            }
            normalized.push(character);
            separator_pending = false;
        } else {
            separator_pending = true;
        }
    }
    if normalized.is_empty() {
        Err("model operation slot must be non-empty".to_string())
    } else {
        Ok(normalized)
    }
}
