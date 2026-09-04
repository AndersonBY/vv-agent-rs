//! Pure checkpoint transitions for durable deferred tool batches.

use crate::checkpoint::{
    canonical_json_bytes, validate_definitive_result, CheckpointError, CheckpointResult,
    CheckpointStatus, DeferredBatchEntry, DeferredToolHandle, OperationState, ToolCallOutcome,
};
use crate::events::{EventId, RunEvent, RunEventPayload, ToolStatus};
use crate::runtime::state::OperationError;
use crate::types::{ToolExecutionResult, ToolResultStatus};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::Checkpoint;

pub fn admit_deferred_batch(
    current: &Checkpoint,
    expected_revision: u64,
    claim_token: &str,
    claimed_cycle: u64,
    entries: &[DeferredBatchEntry],
) -> CheckpointResult<(Checkpoint, Vec<DeferredToolHandle>)> {
    if current.revision != expected_revision
        || current.claim_token.as_deref() != Some(claim_token)
        || current.claimed_cycle != Some(claimed_cycle)
        || current.status != CheckpointStatus::Running
        || entries.is_empty()
    {
        return Ok((current.clone(), Vec::new()));
    }
    let mut snapshot = current.clone();
    let mut handles = Vec::new();
    let mut deferred_count = 0usize;
    for batch in entries {
        batch.validate()?;
        let Some(index) = snapshot
            .tool_journal
            .iter()
            .position(|entry| entry.operation_id == batch.operation_id)
        else {
            return Err(CheckpointError::new(
                "deferred_batch_not_admitted",
                "deferred batch operation is not in the active tool journal",
            ));
        };
        let journal = &mut snapshot.tool_journal[index];
        if journal.state != OperationState::Started
            || journal.cycle_index != batch.cycle_index
            || journal.attempt != batch.attempt
            || journal.request_digest != batch.request_digest
            || journal.tool_call_id.as_deref() != Some(batch.tool_call_id.as_str())
            || journal.tool_name.as_deref() != Some(batch.tool_name.as_str())
        {
            return Err(CheckpointError::new(
                "deferred_batch_not_admitted",
                "deferred batch operation identity or state does not match",
            ));
        }
        match &batch.outcome {
            ToolCallOutcome::Deferred { handle } => {
                if handle.checkpoint_key != snapshot.checkpoint_key
                    || handle.operation_id != batch.operation_id
                    || handle.attempt != batch.attempt
                    || handle.request_digest != batch.request_digest
                {
                    return Err(CheckpointError::new(
                        "deferred_handle_invalid",
                        "deferred handle does not match the active journal",
                    ));
                }
                journal.state = OperationState::Deferred;
                journal.deferred_handle = Some(handle.clone());
                handles.push(handle.clone());
                deferred_count += 1;
                let event = deferred_event(&snapshot, batch, handle)?;
                snapshot.event_outbox.push(event);
            }
            ToolCallOutcome::Completed { result } => {
                if result.tool_call_id != batch.tool_call_id {
                    return Err(CheckpointError::new(
                        "deferred_batch_result_invalid",
                        "completed result tool_call_id does not match the journal",
                    ));
                }
                validate_definitive_result(result).map_err(|error| {
                    if error.code() == "deferred_resolution_result_invalid" {
                        CheckpointError::new("deferred_batch_result_invalid", error.message())
                    } else {
                        error
                    }
                })?;
                match result.status {
                    ToolResultStatus::Success => {
                        journal.result = Some(result.to_dict());
                        journal.state = OperationState::Succeeded;
                    }
                    ToolResultStatus::Error => {
                        journal.error = Some(OperationError::new(
                            result
                                .error_code
                                .clone()
                                .unwrap_or_else(|| "tool_error".to_string()),
                            result.content.clone(),
                            false,
                        ));
                        journal.state = OperationState::Failed;
                    }
                    _ => {
                        return Err(CheckpointError::new(
                            "deferred_batch_result_invalid",
                            "admission completed result must be SUCCESS or ERROR",
                        ));
                    }
                }
                let event = completed_event(&snapshot, batch, result, None)?;
                snapshot.event_outbox.push(event);
            }
        }
    }
    // The admission boundary classifies the complete model-tool batch.  A
    // caller that omits one still-started tool would otherwise release the
    // claim while leaving an external operation unclassified.
    if has_current_cycle_state(
        snapshot
            .model_call_journal
            .iter()
            .chain(snapshot.tool_journal.iter()),
        claimed_cycle,
        &[OperationState::Started],
    ) {
        return Err(CheckpointError::new(
            "deferred_batch_incomplete",
            "deferred batch must cover every started tool in the claimed cycle",
        ));
    }
    if deferred_count > 0 {
        snapshot.status = CheckpointStatus::Deferred;
        snapshot.claim_token = None;
        snapshot.claimed_cycle = None;
        snapshot.lease_expires_at_ms = None;
    }
    snapshot.revision = expected_revision
        .checked_add(1)
        .ok_or_else(|| CheckpointError::new("checkpoint_revision_overflow", "revision overflow"))?;
    snapshot.validate()?;
    Ok((snapshot, handles))
}

pub fn accept_deferred_batch(
    current: &Checkpoint,
    expected_revision: u64,
    claim_token: &str,
    claimed_cycle: u64,
    decisions: &[crate::checkpoint::AcceptDeferredDecision],
) -> CheckpointResult<(Checkpoint, bool)> {
    if current.revision != expected_revision
        || current.claim_token.as_deref() != Some(claim_token)
        || current.claimed_cycle != Some(claimed_cycle)
        // `accept_deferred_batch` is an authority decision over an ambiguous
        // recovery snapshot.  A normal continuation claim must never be able
        // to turn an arbitrary ambiguous operation into a deferred receipt.
        || current.resume_attempt <= 1
        || decisions.is_empty()
    {
        return Ok((current.clone(), false));
    }
    if has_current_cycle_state(
        current.model_call_journal.iter(),
        claimed_cycle,
        &[
            OperationState::Planned,
            OperationState::Started,
            OperationState::Ambiguous,
        ],
    ) {
        return Err(CheckpointError::new(
            "reconciliation_required",
            "accept_deferred batch must cover model and tool ambiguity before release",
        ));
    }
    let mut decision_keys = std::collections::BTreeSet::new();
    let mut snapshot = current.clone();
    let mut accepted = 0usize;
    for decision in decisions {
        decision.validate()?;
        if !decision_keys.insert((
            decision.handle.operation_id.clone(),
            decision.handle.attempt,
            decision.handle.request_digest.clone(),
        )) {
            return Err(CheckpointError::new(
                "reconciliation_required",
                "accept_deferred batch contains duplicate handles",
            ));
        }
        let Some(index) = snapshot.tool_journal.iter().position(|entry| {
            entry.operation_id == decision.handle.operation_id
                && entry.cycle_index == claimed_cycle
                && entry.attempt == decision.handle.attempt
                && entry.request_digest == decision.handle.request_digest
                && entry.state == OperationState::Ambiguous
        }) else {
            return Err(CheckpointError::new(
                "reconciliation_required",
                "accept_deferred decision does not match an ambiguous current-batch entry",
            ));
        };
        if decision.handle.checkpoint_key != snapshot.checkpoint_key {
            return Err(CheckpointError::new(
                "reconciliation_required",
                "accept_deferred handle checkpoint does not match the store key",
            ));
        }
        let entry_snapshot = {
            let entry = &mut snapshot.tool_journal[index];
            if decision.handle.operation_id != entry.operation_id
                || decision.handle.attempt != entry.attempt
                || decision.handle.request_digest != entry.request_digest
            {
                return Err(CheckpointError::new(
                    "reconciliation_required",
                    "accept_deferred handle identity does not match the journal",
                ));
            }
            entry.state = OperationState::Deferred;
            entry.deferred_handle = Some(decision.handle.clone());
            entry.clone()
        };
        accepted += 1;
        let reconciliation = reconciliation_event(&snapshot, &entry_snapshot)?;
        let deferred = deferred_event_from_journal(&snapshot, &entry_snapshot)?;
        snapshot.event_outbox.push(reconciliation);
        snapshot.event_outbox.push(deferred);
    }
    if accepted == 0 {
        return Ok((current.clone(), false));
    }
    // Recovery acceptance is a barrier for the complete current model-tool
    // batch.  Releasing the recovery claim while another ambiguous entry is
    // left behind would falsely turn a partial authority decision into a
    // deferred barrier.
    if has_current_cycle_state(
        snapshot
            .model_call_journal
            .iter()
            .chain(snapshot.tool_journal.iter()),
        claimed_cycle,
        &[
            OperationState::Planned,
            OperationState::Started,
            OperationState::Ambiguous,
        ],
    ) {
        return Err(CheckpointError::new(
            "reconciliation_required",
            "accept_deferred batch must cover every unresolved journal entry in the current cycle",
        ));
    }
    snapshot.status = CheckpointStatus::Deferred;
    snapshot.claim_token = None;
    snapshot.claimed_cycle = None;
    snapshot.lease_expires_at_ms = None;
    snapshot.revision = expected_revision
        .checked_add(1)
        .ok_or_else(|| CheckpointError::new("checkpoint_revision_overflow", "revision overflow"))?;
    snapshot.validate()?;
    Ok((snapshot, true))
}

fn has_current_cycle_state<'a>(
    mut entries: impl Iterator<Item = &'a crate::runtime::state::OperationJournalEntry>,
    claimed_cycle: u64,
    states: &[OperationState],
) -> bool {
    entries.any(|entry| entry.cycle_index == claimed_cycle && states.contains(&entry.state))
}

/// Returns true when a reconciliation decision batch is an exact replay of
/// handles already adopted into the deferred barrier. Replays are observable
/// reads and must not require a new recovery claim or revision.
pub fn deferred_batch_is_idempotent(
    current: &Checkpoint,
    decisions: &[crate::checkpoint::AcceptDeferredDecision],
) -> bool {
    if decisions.is_empty() {
        return false;
    }
    let mut decision_keys = std::collections::BTreeSet::new();
    if decisions.iter().any(|decision| {
        !decision_keys.insert((
            decision.handle.operation_id.clone(),
            decision.handle.attempt,
            decision.handle.request_digest.clone(),
        ))
    }) {
        return false;
    }
    let deferred = current
        .tool_journal
        .iter()
        .filter(|entry| entry.state == OperationState::Deferred)
        .collect::<Vec<_>>();
    deferred.len() == decisions.len()
        && deferred.iter().all(|entry| {
            entry.deferred_handle.as_ref().is_some_and(|handle| {
                decision_keys.contains(&(
                    handle.operation_id.clone(),
                    handle.attempt,
                    handle.request_digest.clone(),
                )) && decisions.iter().any(|decision| &decision.handle == handle)
            })
        })
}

fn deferred_event(
    checkpoint: &Checkpoint,
    batch: &DeferredBatchEntry,
    handle: &DeferredToolHandle,
) -> CheckpointResult<crate::runtime::state::EventOutboxEntry> {
    let mut event = RunEvent::tool_call_deferred(
        checkpoint.root_run_id.clone(),
        checkpoint.trace_id.clone(),
        checkpoint.task_id.clone(),
        batch.cycle_index as u32,
        batch.tool_call_id.clone(),
        batch.tool_name.clone(),
        batch.operation_id.clone(),
        batch.attempt as u32,
        handle.clone(),
        true,
        None,
    );
    event.event_id = EventId::stable(format!("evt_deferred_{}_deferred", batch.tool_call_id))
        .map_err(|error| CheckpointError::new("checkpoint_event_invalid", error))?;
    let mut value = serde_json::to_value(event)
        .map_err(|error| CheckpointError::new("checkpoint_event_invalid", error.to_string()))?;
    let object = value
        .as_object_mut()
        .expect("serialized RunEvent must be an object");
    object.remove("agent_name");
    // These optional admission fields are part of the v4 resume-event
    // projection.  Keep them symmetric with reconciliation events while the
    // handle remains the source of exact identity.
    object.insert(
        "checkpoint_key".to_string(),
        Value::String(checkpoint.checkpoint_key.clone()),
    );
    object.insert(
        "operation_kind".to_string(),
        Value::String("tool".to_string()),
    );
    crate::runtime::state::EventOutboxEntry::pending(
        value
            .get("event_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        value,
    )
}

fn deferred_event_from_journal(
    checkpoint: &Checkpoint,
    journal: &crate::runtime::state::OperationJournalEntry,
) -> CheckpointResult<crate::runtime::state::EventOutboxEntry> {
    let handle = journal.deferred_handle.as_ref().ok_or_else(|| {
        CheckpointError::new(
            "operation_deferred_handle_required",
            "deferred handle missing",
        )
    })?;
    let batch = DeferredBatchEntry {
        operation_id: journal.operation_id.clone(),
        cycle_index: journal.cycle_index,
        attempt: journal.attempt,
        request_digest: journal.request_digest.clone(),
        tool_call_id: journal.tool_call_id.clone().unwrap_or_default(),
        tool_name: journal.tool_name.clone().unwrap_or_default(),
        idempotency_key: journal.idempotency_key.clone(),
        idempotency_support: journal.idempotency_support.unwrap_or_default(),
        outcome: ToolCallOutcome::deferred(handle.clone()),
    };
    deferred_event(checkpoint, &batch, handle)
}

fn reconciliation_event(
    checkpoint: &Checkpoint,
    journal: &crate::runtime::state::OperationJournalEntry,
) -> CheckpointResult<crate::runtime::state::EventOutboxEntry> {
    let mut event = RunEvent::new(
        checkpoint.root_run_id.clone(),
        checkpoint.trace_id.clone(),
        checkpoint.task_id.clone(),
        Some(journal.cycle_index as u32),
        RunEventPayload::ReconciliationResolved {
            checkpoint_key: checkpoint.checkpoint_key.clone(),
            operation_id: journal.operation_id.clone(),
            operation_kind: crate::checkpoint::OperationKind::Tool,
            decision: crate::checkpoint::ReconciliationDecisionKind::AcceptDeferred,
            claim_mode: Some(crate::checkpoint::ClaimMode::Recovery),
        },
    );
    event.event_id = EventId::stable(format!(
        "evt_deferred_{}_reconciliation",
        journal
            .tool_call_id
            .as_deref()
            .unwrap_or(&journal.operation_id)
    ))
    .map_err(|error| CheckpointError::new("checkpoint_event_invalid", error))?;
    outbox(event)
}

fn completed_event(
    checkpoint: &Checkpoint,
    batch: &DeferredBatchEntry,
    result: &ToolExecutionResult,
    event_id: Option<String>,
) -> CheckpointResult<crate::runtime::state::EventOutboxEntry> {
    let status = match result.status {
        ToolResultStatus::Success => ToolStatus::Success,
        ToolResultStatus::Error => ToolStatus::Error,
        ToolResultStatus::WaitResponse => ToolStatus::WaitResponse,
        ToolResultStatus::Running => ToolStatus::Running,
        ToolResultStatus::PendingCompress => ToolStatus::PendingCompress,
    };
    let mut event = RunEvent::new(
        checkpoint.root_run_id.clone(),
        checkpoint.trace_id.clone(),
        checkpoint.task_id.clone(),
        Some(batch.cycle_index as u32),
        RunEventPayload::ToolCallCompleted {
            tool_call_id: batch.tool_call_id.clone(),
            tool_name: batch.tool_name.clone(),
            status,
            directive: result.directive,
            error_code: result.error_code.clone(),
            execution_started: true,
            duration_ms: None,
            operation_id: Some(batch.operation_id.clone()),
            attempt: Some(batch.attempt as u32),
        },
    );
    let id = event_id.unwrap_or_else(|| {
        format!(
            "evt_deferred_{}_{}",
            batch.tool_call_id,
            if result.status == ToolResultStatus::Success {
                "completed"
            } else {
                "failed"
            }
        )
    });
    event.event_id = EventId::stable(id)
        .map_err(|error| CheckpointError::new("checkpoint_event_invalid", error))?;
    outbox(event)
}

fn outbox(event: RunEvent) -> CheckpointResult<crate::runtime::state::EventOutboxEntry> {
    let mut value = serde_json::to_value(event)
        .map_err(|error| CheckpointError::new("checkpoint_event_invalid", error.to_string()))?;
    // Deferred receipts are checkpoint-scoped and do not have an agent-name
    // projection.  The canonical event payload intentionally omits this
    // optional field (the generic RunEvent constructor supplies one for live
    // runtime events).
    value
        .as_object_mut()
        .expect("serialized RunEvent must be an object")
        .remove("agent_name");
    crate::runtime::state::EventOutboxEntry::pending(
        value
            .get("event_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        value,
    )
}

pub fn receipt_event(
    checkpoint: &Checkpoint,
    journal: &crate::runtime::state::OperationJournalEntry,
    result: &ToolExecutionResult,
) -> CheckpointResult<crate::runtime::state::EventOutboxEntry> {
    let batch = DeferredBatchEntry {
        operation_id: journal.operation_id.clone(),
        cycle_index: journal.cycle_index,
        attempt: journal.attempt,
        request_digest: journal.request_digest.clone(),
        tool_call_id: journal.tool_call_id.clone().unwrap_or_default(),
        tool_name: journal.tool_name.clone().unwrap_or_default(),
        idempotency_key: journal.idempotency_key.clone(),
        idempotency_support: journal.idempotency_support.unwrap_or_default(),
        outcome: ToolCallOutcome::completed(result.clone()),
    };
    let event_id = format!(
        "evt_deferred_{}_{}",
        journal
            .tool_call_id
            .as_deref()
            .unwrap_or(&journal.operation_id),
        if result.status == ToolResultStatus::Success {
            "completed"
        } else {
            "failed"
        }
    );
    completed_event(checkpoint, &batch, result, Some(event_id))
}

pub fn handle_key(value: &DeferredToolHandle) -> CheckpointResult<String> {
    let bytes = canonical_json_bytes(
        &serde_json::to_value(value)
            .map_err(|error| CheckpointError::new("deferred_handle_invalid", error.to_string()))?,
        "deferred handle",
    )?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
