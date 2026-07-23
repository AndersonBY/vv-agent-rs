use std::collections::HashSet;

use crate::budget::BudgetEnforcementBoundary;
use crate::events::{ModelCallFailureOutcome, RunEventPayload};
use crate::types::{AgentResult, AgentStatus, ModelCallStatus};

use super::*;

pub fn validate_checkpoint(checkpoint: &Checkpoint) -> CheckpointResult<()> {
    if checkpoint.schema_version != CHECKPOINT_SCHEMA {
        return Err(CheckpointError::new(
            "checkpoint_schema_unsupported",
            "checkpoint schema_version is unsupported",
        ));
    }
    if checkpoint.run_definition_schema != RUN_DEFINITION_SCHEMA {
        return Err(CheckpointError::new(
            "checkpoint_definition_schema_unsupported",
            "run_definition_schema is missing or unsupported",
        ));
    }
    crate::checkpoint::validate_run_definition(&checkpoint.run_definition)?;
    validate_sha256(&checkpoint.run_definition_digest, "run_definition_digest").map_err(
        |error| CheckpointError::new("checkpoint_definition_digest_invalid", error.message()),
    )?;
    let digest = crate::checkpoint::run_definition_digest(&checkpoint.run_definition)?;
    if digest != checkpoint.run_definition_digest {
        return Err(CheckpointError::new(
            "checkpoint_definition_mismatch",
            "run_definition_digest does not match embedded run_definition",
        ));
    }
    validate_checkpoint_key(&checkpoint.checkpoint_key)?;
    for (value, field_name) in [
        (&checkpoint.task_id, "task_id"),
        (&checkpoint.root_run_id, "root_run_id"),
        (&checkpoint.trace_id, "trace_id"),
    ] {
        if value.trim().is_empty() {
            return Err(CheckpointError::new(
                "checkpoint_value_invalid",
                format!("{field_name} must be non-empty"),
            ));
        }
    }
    if checkpoint.resume_attempt == 0 || checkpoint.resume_attempt > MAX_WIRE_INTEGER {
        return Err(CheckpointError::new(
            "checkpoint_resume_attempt_invalid",
            "resume_attempt must be positive and JSON-safe",
        ));
    }
    if checkpoint.cycle_index > MAX_WIRE_INTEGER || checkpoint.revision > MAX_WIRE_INTEGER {
        return Err(CheckpointError::new(
            "checkpoint_integer_invalid",
            "checkpoint integer is outside the JSON-safe range",
        ));
    }
    let claim_values = [
        checkpoint.claim_token.is_some(),
        checkpoint.claimed_cycle.is_some(),
        checkpoint.lease_expires_at_ms.is_some(),
    ];
    if claim_values.iter().any(|value| *value) && claim_values.iter().any(|value| !*value) {
        return Err(CheckpointError::new(
            "checkpoint_claim_invalid",
            "claim fields must be all present or all null",
        ));
    }
    if let Some(claim_token) = &checkpoint.claim_token {
        if claim_token.trim().is_empty() {
            return Err(CheckpointError::new(
                "checkpoint_claim_invalid",
                "claim_token must be non-empty",
            ));
        }
        let claimed_cycle = checkpoint.claimed_cycle.expect("claim tuple checked");
        let expected = checkpoint.cycle_index.checked_add(1).ok_or_else(|| {
            CheckpointError::new("checkpoint_claim_invalid", "claimed cycle overflow")
        })?;
        if claimed_cycle != expected || claimed_cycle == 0 || claimed_cycle > MAX_WIRE_INTEGER {
            return Err(CheckpointError::new(
                "checkpoint_claim_invalid",
                "claimed_cycle must equal cycle_index + 1",
            ));
        }
        let lease = checkpoint.lease_expires_at_ms.expect("claim tuple checked");
        if lease > MAX_WIRE_INTEGER {
            return Err(CheckpointError::new(
                "checkpoint_claim_invalid",
                "lease expiry is outside the JSON-safe range",
            ));
        }
    }
    if checkpoint.terminal_result.is_some() && checkpoint.claim_token.is_some() {
        return Err(CheckpointError::new(
            "checkpoint_status_invalid",
            "terminal checkpoint cannot have an active claim",
        ));
    }
    if checkpoint.terminal_acknowledged && checkpoint.terminal_result.is_none() {
        return Err(CheckpointError::new(
            "checkpoint_status_invalid",
            "terminal acknowledgement requires a terminal result",
        ));
    }
    if checkpoint.terminal_result.is_none()
        && !matches!(
            checkpoint.status,
            CheckpointStatus::Running | CheckpointStatus::ReconciliationRequired
        )
    {
        return Err(CheckpointError::new(
            "checkpoint_status_invalid",
            "non-terminal checkpoint must be running or reconciliation_required",
        ));
    }
    if checkpoint.terminal_result.is_some() && !checkpoint.status.is_terminal() {
        return Err(CheckpointError::new(
            "checkpoint_status_invalid",
            "terminal_result requires a terminal checkpoint status",
        ));
    }
    let call_ids = checkpoint
        .model_calls
        .iter()
        .map(|record| record.call_id.as_str())
        .collect::<HashSet<_>>();
    if call_ids.len() != checkpoint.model_calls.len() {
        return Err(CheckpointError::new(
            "checkpoint_status_invalid",
            "checkpoint model_calls contains duplicate call ids",
        ));
    }

    let active_cycle = checkpoint.active_cycle()?;
    for entry in checkpoint
        .model_call_journal
        .iter()
        .chain(checkpoint.tool_journal.iter())
    {
        entry.validate()?;
        if entry.cycle_index != active_cycle {
            return Err(CheckpointError::new(
                "checkpoint_journal_cycle_invalid",
                "journal cycle_index must equal the active cycle",
            ));
        }
    }
    if checkpoint
        .model_call_journal
        .iter()
        .any(|entry| entry.kind != OperationKind::Model)
        || checkpoint
            .tool_journal
            .iter()
            .any(|entry| entry.kind != OperationKind::Tool)
    {
        return Err(CheckpointError::new(
            "checkpoint_journal_kind_invalid",
            "journal arrays contain an entry of the wrong kind",
        ));
    }
    for (namespace, entry) in &checkpoint.extension_state {
        validate_extension_namespace(namespace)?;
        entry.validate()?;
    }
    validate_extension_state_size(&checkpoint.extension_state, MAX_WIRE_INTEGER)?;
    if let Some(cursor) = &checkpoint.event_cursor {
        cursor.validate()?;
    }
    let mut event_ids = HashSet::new();
    for entry in &checkpoint.event_outbox {
        entry.verify_payload()?;
        if !event_ids.insert(entry.event_id.as_str()) {
            return Err(CheckpointError::new(
                "event_identity_conflict",
                "checkpoint event_outbox contains a duplicate event id",
            ));
        }
    }
    validate_model_journal_accounting(checkpoint)?;
    for value in checkpoint.shared_state.values() {
        validate_json(value, "shared_state")?;
    }
    if checkpoint.status == CheckpointStatus::ReconciliationRequired
        && (!checkpoint.has_ambiguous_operation() || checkpoint.claim_token.is_some())
    {
        return Err(CheckpointError::new(
            "checkpoint_status_invalid",
            "reconciliation_required needs an ambiguous journal and no claim",
        ));
    }
    if checkpoint.status == CheckpointStatus::Running
        && checkpoint.has_ambiguous_operation()
        && checkpoint.claim_token.is_none()
    {
        return Err(CheckpointError::new(
            "checkpoint_status_invalid",
            "running checkpoint with ambiguity needs an active recovery claim",
        ));
    }
    if checkpoint.terminal_result.is_some()
        && (!checkpoint.model_call_journal.is_empty() || !checkpoint.tool_journal.is_empty())
        && !checkpoint.is_operator_abort_terminal()
    {
        return Err(CheckpointError::new(
            "checkpoint_status_invalid",
            "terminal checkpoint cannot retain active journals",
        ));
    }
    if let Some(result) = &checkpoint.terminal_result {
        validate_json(result, "terminal_result")?;
        let result = AgentResult::from_dict(result).map_err(|error| {
            CheckpointError::new(
                "checkpoint_status_invalid",
                format!("terminal_result is not the current AgentResult shape: {error}"),
            )
        })?;
        if !agent_status_matches_checkpoint(result.status, checkpoint.status) {
            return Err(CheckpointError::new(
                "checkpoint_status_invalid",
                "terminal result status must match checkpoint status",
            ));
        }
        if result
            .checkpoint_key
            .as_deref()
            .is_some_and(|key| key != checkpoint.checkpoint_key)
        {
            return Err(CheckpointError::new(
                "checkpoint_status_invalid",
                "terminal result checkpoint_key must match checkpoint",
            ));
        }
        if result.token_usage.model_calls != checkpoint.model_calls {
            return Err(CheckpointError::new(
                "checkpoint_status_invalid",
                "terminal result model-call ledger does not match checkpoint",
            ));
        }
    }
    Ok(())
}

pub fn validate_model_journal_accounting(checkpoint: &Checkpoint) -> CheckpointResult<()> {
    for journal in &checkpoint.model_call_journal {
        if journal.kind != OperationKind::Model {
            return Err(CheckpointError::new(
                "operation_kind_fields_invalid",
                "model_call_journal contains a non-model entry",
            ));
        }
        validate_model_journal_entry_accounting(checkpoint, journal)?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ModelAccountingIdentity {
    call_id: String,
    operation_id: String,
    attempt: u64,
    operation: ModelCallOperation,
    cycle_index: u64,
    backend: String,
    model: String,
}

fn validate_model_journal_entry_accounting(
    checkpoint: &Checkpoint,
    journal: &OperationJournalEntry,
) -> CheckpointResult<()> {
    journal.validate()?;
    let identity = model_journal_identity(journal)?;
    let record_candidates = checkpoint
        .model_calls
        .iter()
        .filter(|record| {
            record.call_id == identity.call_id
                || (record.operation_id == identity.operation_id
                    && u64::from(record.attempt) == identity.attempt)
        })
        .collect::<Vec<_>>();

    let mut event_candidates = Vec::new();
    for (index, entry) in checkpoint.event_outbox.iter().enumerate() {
        entry.verify_payload()?;
        let event: RunEvent = serde_json::from_value(entry.event.clone()).map_err(|error| {
            CheckpointError::new(
                "checkpoint_event_outbox_invalid",
                format!("checkpoint event payload is invalid: {error}"),
            )
        })?;
        let Some(event_identity) = model_event_identity(&event) else {
            continue;
        };
        if event_identity.call_id == identity.call_id
            || (event_identity.operation_id == identity.operation_id
                && event_identity.attempt == identity.attempt)
        {
            event_candidates.push((index, event, event_identity));
        }
    }
    let started_events = event_candidates
        .iter()
        .filter(|(_, event, _)| matches!(event.payload(), RunEventPayload::ModelCallStarted { .. }))
        .collect::<Vec<_>>();
    let terminal_events = event_candidates
        .iter()
        .filter(|(_, event, _)| {
            matches!(
                event.payload(),
                RunEventPayload::ModelCallCompleted { .. }
                    | RunEventPayload::ModelCallFailed { .. }
            )
        })
        .collect::<Vec<_>>();

    if record_candidates.len() > 1 || started_events.len() > 1 || terminal_events.len() > 1 {
        return Err(model_accounting_error(
            "model journal attempt has duplicate accounting evidence",
        ));
    }

    match journal.state {
        OperationState::Planned => {
            require_model_evidence_counts(
                &record_candidates,
                &started_events,
                &terminal_events,
                (0, 0, 0),
            )?;
            return Ok(());
        }
        OperationState::Started => {
            require_model_evidence_counts(
                &record_candidates,
                &started_events,
                &terminal_events,
                (0, 1, 0),
            )?;
            require_model_identity(&identity, &started_events[0].2)?;
            return Ok(());
        }
        OperationState::Failed
            if record_candidates.is_empty()
                && started_events.is_empty()
                && terminal_events.is_empty() =>
        {
            return Ok(());
        }
        OperationState::Succeeded | OperationState::Failed | OperationState::Ambiguous => {}
    }

    require_model_evidence_counts(
        &record_candidates,
        &started_events,
        &terminal_events,
        (1, 1, 1),
    )?;
    let record = record_candidates[0];
    let started_event = started_events[0];
    let terminal_event = terminal_events[0];
    require_model_identity(&identity, &model_record_identity(record))?;
    require_model_identity(&identity, &started_event.2)?;
    require_model_identity(&identity, &terminal_event.2)?;

    let status_matches = match journal.state {
        OperationState::Succeeded => matches!(
            record.status,
            ModelCallStatus::Completed | ModelCallStatus::Ambiguous
        ),
        OperationState::Failed => matches!(
            record.status,
            ModelCallStatus::Failed | ModelCallStatus::Ambiguous
        ),
        OperationState::Ambiguous => record.status == ModelCallStatus::Ambiguous,
        OperationState::Planned | OperationState::Started => false,
    };
    let event_type_matches = matches!(
        (record.status, terminal_event.1.payload()),
        (
            ModelCallStatus::Completed,
            RunEventPayload::ModelCallCompleted { .. }
        ) | (
            ModelCallStatus::Failed | ModelCallStatus::Ambiguous,
            RunEventPayload::ModelCallFailed { .. }
        )
    );
    if !status_matches || !event_type_matches {
        return Err(model_accounting_error(
            "model journal terminal state does not match its accounting evidence",
        ));
    }

    let event_usage = match terminal_event.1.payload() {
        RunEventPayload::ModelCallCompleted { usage, .. }
        | RunEventPayload::ModelCallFailed { usage, .. } => usage,
        _ => unreachable!("terminal event filtered above"),
    };
    if event_usage != &record.usage {
        return Err(model_accounting_error(
            "model terminal event usage does not match its ledger record",
        ));
    }
    if let RunEventPayload::ModelCallFailed {
        outcome,
        error_code,
        ..
    } = terminal_event.1.payload()
    {
        let expected_outcome = if record.status == ModelCallStatus::Ambiguous {
            ModelCallFailureOutcome::Ambiguous
        } else {
            ModelCallFailureOutcome::Definitive
        };
        if *outcome != expected_outcome || record.error_code.as_deref() != Some(error_code.as_str())
        {
            return Err(model_accounting_error(
                "model failed event does not match its ledger record",
            ));
        }
    }

    validate_terminal_budget_event_order(checkpoint, terminal_event.0)?;
    Ok(())
}

fn require_model_evidence_counts(
    records: &[&ModelCallRecord],
    started_events: &[&(usize, RunEvent, ModelAccountingIdentity)],
    terminal_events: &[&(usize, RunEvent, ModelAccountingIdentity)],
    expected: (usize, usize, usize),
) -> CheckpointResult<()> {
    if (records.len(), started_events.len(), terminal_events.len()) != expected {
        return Err(model_accounting_error(
            "model journal attempt is missing atomic accounting evidence",
        ));
    }
    Ok(())
}

fn model_journal_identity(
    journal: &OperationJournalEntry,
) -> CheckpointResult<ModelAccountingIdentity> {
    Ok(ModelAccountingIdentity {
        call_id: journal.call_id.clone().ok_or_else(|| {
            model_accounting_error("model journal call_id is missing from accounting identity")
        })?,
        operation_id: journal.operation_id.clone(),
        attempt: journal.attempt,
        operation: journal.model_operation.ok_or_else(|| {
            model_accounting_error("model journal operation is missing from accounting identity")
        })?,
        cycle_index: journal.cycle_index,
        backend: journal.backend.clone().ok_or_else(|| {
            model_accounting_error("model journal backend is missing from accounting identity")
        })?,
        model: journal.model.clone().ok_or_else(|| {
            model_accounting_error("model journal model is missing from accounting identity")
        })?,
    })
}

fn model_record_identity(record: &ModelCallRecord) -> ModelAccountingIdentity {
    ModelAccountingIdentity {
        call_id: record.call_id.clone(),
        operation_id: record.operation_id.clone(),
        attempt: u64::from(record.attempt),
        operation: record.operation,
        cycle_index: u64::from(record.cycle_index),
        backend: record.backend.clone(),
        model: record.model.clone(),
    }
}

fn model_event_identity(event: &RunEvent) -> Option<ModelAccountingIdentity> {
    let (call_id, operation_id, attempt, operation, backend, model) = match event.payload() {
        RunEventPayload::ModelCallStarted {
            call_id,
            operation_id,
            attempt,
            operation,
            backend,
            model,
        }
        | RunEventPayload::ModelCallCompleted {
            call_id,
            operation_id,
            attempt,
            operation,
            backend,
            model,
            ..
        }
        | RunEventPayload::ModelCallFailed {
            call_id,
            operation_id,
            attempt,
            operation,
            backend,
            model,
            ..
        } => (call_id, operation_id, attempt, operation, backend, model),
        _ => return None,
    };
    Some(ModelAccountingIdentity {
        call_id: call_id.clone(),
        operation_id: operation_id.clone(),
        attempt: u64::from(*attempt),
        operation: *operation,
        cycle_index: u64::from(event.cycle_index()?),
        backend: backend.clone(),
        model: model.clone(),
    })
}

fn require_model_identity(
    expected: &ModelAccountingIdentity,
    observed: &ModelAccountingIdentity,
) -> CheckpointResult<()> {
    if expected != observed {
        return Err(model_accounting_error(
            "model journal, event, and ledger identities do not match",
        ));
    }
    Ok(())
}

fn validate_terminal_budget_event_order(
    checkpoint: &Checkpoint,
    terminal_event_index: usize,
) -> CheckpointResult<()> {
    let budget_configured = checkpoint
        .run_definition
        .get("budget_limits")
        .is_some_and(|value| !value.is_null());
    let next_event = checkpoint
        .event_outbox
        .get(terminal_event_index + 1)
        .map(|entry| {
            serde_json::from_value::<RunEvent>(entry.event.clone()).map_err(|error| {
                CheckpointError::new(
                    "checkpoint_event_outbox_invalid",
                    format!("checkpoint event payload is invalid: {error}"),
                )
            })
        })
        .transpose()?;
    let next_is_model_budget = next_event.as_ref().is_some_and(|event| {
        matches!(
            event.payload(),
            RunEventPayload::BudgetSnapshot {
                enforcement_boundary: BudgetEnforcementBoundary::ModelCallComplete,
                ..
            } | RunEventPayload::BudgetExhausted {
                enforcement_boundary: BudgetEnforcementBoundary::ModelCallComplete,
                ..
            }
        )
    });
    if budget_configured != next_is_model_budget {
        return Err(model_accounting_error(
            "model terminal event must be followed immediately by its configured budget observation",
        ));
    }
    if next_is_model_budget {
        let duplicate_budget = checkpoint
            .event_outbox
            .get(terminal_event_index + 2)
            .map(|entry| {
                serde_json::from_value::<RunEvent>(entry.event.clone()).map_err(|error| {
                    CheckpointError::new(
                        "checkpoint_event_outbox_invalid",
                        format!("checkpoint event payload is invalid: {error}"),
                    )
                })
            })
            .transpose()?
            .as_ref()
            .is_some_and(|event| {
                matches!(
                    event.payload(),
                    RunEventPayload::BudgetSnapshot {
                        enforcement_boundary: BudgetEnforcementBoundary::ModelCallComplete,
                        ..
                    } | RunEventPayload::BudgetExhausted {
                        enforcement_boundary: BudgetEnforcementBoundary::ModelCallComplete,
                        ..
                    }
                )
            });
        if duplicate_budget {
            return Err(model_accounting_error(
                "budget_exhausted must replace budget_snapshot for a model-call boundary",
            ));
        }
    }
    Ok(())
}

fn model_accounting_error(message: impl Into<String>) -> CheckpointError {
    CheckpointError::new("checkpoint_status_invalid", message)
}

fn agent_status_matches_checkpoint(status: AgentStatus, checkpoint: CheckpointStatus) -> bool {
    matches!(
        (status, checkpoint),
        (AgentStatus::Pending, CheckpointStatus::Pending)
            | (AgentStatus::Running, CheckpointStatus::Running)
            | (AgentStatus::WaitUser, CheckpointStatus::WaitUser)
            | (AgentStatus::Completed, CheckpointStatus::Completed)
            | (AgentStatus::Failed, CheckpointStatus::Failed)
            | (AgentStatus::MaxCycles, CheckpointStatus::MaxCycles)
            | (
                AgentStatus::ReconciliationRequired,
                CheckpointStatus::ReconciliationRequired
            )
    )
}

pub fn validate_extension_state_size(
    extensions: &BTreeMap<String, ExtensionStateEntry>,
    max_total: u64,
) -> CheckpointResult<()> {
    let mut total = 0_u64;
    for (namespace, entry) in extensions {
        let bytes = canonical_json_bytes(&entry.to_value(), "extension state entry")?;
        if bytes.len() > MAX_EXTENSION_ENTRY_BYTES {
            return Err(CheckpointError::new(
                "checkpoint_extension_entry_too_large",
                format!("extension {namespace} exceeds {MAX_EXTENSION_ENTRY_BYTES} bytes"),
            ));
        }
        total = total.checked_add(bytes.len() as u64).ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_extension_state_too_large",
                "extension state byte count overflow",
            )
        })?;
    }
    if total > max_total {
        return Err(CheckpointError::new(
            "checkpoint_extension_state_too_large",
            format!("extension state exceeds {max_total} bytes"),
        ));
    }
    Ok(())
}

pub(super) fn validate_json(value: &Value, field_name: &str) -> CheckpointResult<()> {
    crate::checkpoint::canonical_json_bytes(value, field_name).map(|_| ())
}

pub(super) fn required_string<'a>(
    object: &'a Map<String, Value>,
    field: &str,
    code: &str,
) -> CheckpointResult<&'a str> {
    object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| CheckpointError::new(code, format!("{field} must be a string")))
}

pub(super) fn optional_string(
    object: &Map<String, Value>,
    field: &str,
) -> CheckpointResult<Option<String>> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(CheckpointError::new(
            "operation_kind_fields_invalid",
            format!("{field} must be a string or null"),
        )),
    }
}

pub(super) fn required_u64(
    object: &Map<String, Value>,
    field: &str,
    code: &str,
) -> CheckpointResult<u64> {
    object
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| CheckpointError::new(code, format!("{field} must be a JSON-safe integer")))
}
