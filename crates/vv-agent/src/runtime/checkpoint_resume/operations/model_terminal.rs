use super::*;

impl CheckpointResumeController {
    pub(in crate::runtime::checkpoint_resume) fn stable_model_terminal_event(
        &self,
        event: RunEvent,
        event_type: &str,
        identity: &ModelCallIdentity,
    ) -> CheckpointResult<RunEvent> {
        event
            .with_event_id(self.stable_event_id(
                event_type,
                &[&identity.operation_id, &identity.attempt.to_string()],
            )?)
            .map_err(|error| CheckpointError::new("checkpoint_event_outbox_invalid", error))
    }

    pub(in crate::runtime::checkpoint_resume) fn commit_model_terminal(
        &mut self,
        operation_id: &str,
        terminal: ModelCallTerminal,
        accounting: &ModelCallCoordinator,
    ) -> CheckpointResult<()> {
        let entry = self
            .find_operation(OperationKind::Model, operation_id)
            .ok_or_else(|| {
                CheckpointError::new(
                    "checkpoint_status_invalid",
                    "model terminal journal entry is missing",
                )
            })?;
        require_model_terminal_identity(&entry, &terminal)?;
        let budget_usage = terminal
            .budget
            .event
            .as_ref()
            .and_then(|event| match event.payload() {
                RunEventPayload::BudgetSnapshot { budget_usage, .. }
                | RunEventPayload::BudgetExhausted { budget_usage, .. } => {
                    Some(budget_usage.clone())
                }
                _ => None,
            });
        let ModelCallTerminal {
            record,
            event,
            budget,
        } = terminal;
        {
            let checkpoint = self.require_checkpoint_mut()?;
            if checkpoint
                .model_calls
                .iter()
                .any(|existing| existing.call_id == record.call_id)
            {
                return Err(CheckpointError::new(
                    "checkpoint_status_invalid",
                    "model call ledger contains a duplicate call_id",
                ));
            }
            checkpoint.model_calls.push(record);
            if let Some(budget_usage) = budget_usage {
                checkpoint.budget_usage = Some(budget_usage);
            }
            queue_event(checkpoint, event)?;
            if let Some(event) = budget.event {
                queue_event(checkpoint, event)?;
            }
        }
        self.progress()?;
        let records = self.require_checkpoint()?.model_calls.clone();
        accounting
            .ledger
            .replace(records)
            .map_err(|error| CheckpointError::new("checkpoint_status_invalid", error))?;
        self.deliver_pending_outbox()
    }
}

pub(in crate::runtime::checkpoint_resume) fn model_identity_from_entry(
    entry: &OperationJournalEntry,
) -> CheckpointResult<ModelCallIdentity> {
    if entry.kind != OperationKind::Model {
        return Err(CheckpointError::new(
            "checkpoint_status_invalid",
            "model identity requested for a non-model journal entry",
        ));
    }
    let attempt = u32::try_from(entry.attempt).map_err(|_| {
        CheckpointError::new(
            "checkpoint_status_invalid",
            "model journal attempt is outside the runtime range",
        )
    })?;
    let cycle_index = u32::try_from(entry.cycle_index).map_err(|_| {
        CheckpointError::new(
            "checkpoint_status_invalid",
            "model journal cycle is outside the runtime range",
        )
    })?;
    let identity = ModelCallIdentity::create(
        entry.operation_id.clone(),
        attempt,
        entry.model_operation.ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_status_invalid",
                "model journal operation is missing",
            )
        })?,
        cycle_index,
        entry.backend.clone().ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_status_invalid",
                "model journal backend is missing",
            )
        })?,
        entry.model.clone().ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_status_invalid",
                "model journal model is missing",
            )
        })?,
    )
    .map_err(|error| CheckpointError::new("checkpoint_status_invalid", error))?;
    if entry.call_id.as_deref() != Some(identity.call_id.as_str()) {
        return Err(CheckpointError::new(
            "checkpoint_status_invalid",
            "model journal call_id does not match operation_id and attempt",
        ));
    }
    Ok(identity)
}

pub(super) fn require_effective_model_identity(
    identity: &ModelCallIdentity,
    operation: crate::types::ModelCallOperation,
    backend: &str,
    model: &str,
) -> CheckpointResult<()> {
    if identity.operation != operation || identity.backend != backend || identity.model != model {
        return Err(CheckpointError::new(
            "checkpoint_journal_integrity_mismatch",
            "effective model identity does not match the durable journal",
        ));
    }
    Ok(())
}

fn require_model_terminal_identity(
    entry: &OperationJournalEntry,
    terminal: &ModelCallTerminal,
) -> CheckpointResult<()> {
    let identity = model_identity_from_entry(entry)?;
    let record = &terminal.record;
    let expected_status = match entry.state {
        OperationState::Succeeded => crate::types::ModelCallStatus::Completed,
        OperationState::Failed => crate::types::ModelCallStatus::Failed,
        OperationState::Ambiguous => crate::types::ModelCallStatus::Ambiguous,
        _ => {
            return Err(CheckpointError::new(
                "checkpoint_status_invalid",
                "model terminal requires a terminal journal state",
            ))
        }
    };
    if record.call_id != identity.call_id
        || record.operation_id != identity.operation_id
        || record.attempt != identity.attempt
        || record.operation != identity.operation
        || record.cycle_index != identity.cycle_index
        || record.backend != identity.backend
        || record.model != identity.model
        || record.status != expected_status
    {
        return Err(CheckpointError::new(
            "checkpoint_status_invalid",
            "model terminal record identity does not match its durable journal",
        ));
    }
    let event_matches = match terminal.event.payload() {
        RunEventPayload::ModelCallCompleted {
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
        } => {
            terminal.event.cycle_index() == Some(identity.cycle_index)
                && call_id == &identity.call_id
                && operation_id == &identity.operation_id
                && *attempt == identity.attempt
                && *operation == identity.operation
                && backend == &identity.backend
                && model == &identity.model
        }
        _ => false,
    };
    if !event_matches {
        return Err(CheckpointError::new(
            "checkpoint_status_invalid",
            "model terminal event identity does not match its durable journal",
        ));
    }
    Ok(())
}
