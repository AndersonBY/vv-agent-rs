use crate::checkpoint::{
    CheckpointError, CheckpointResult, HostInteractionAdmissionContext, HostInteractionRequest,
    OperationState, ToolCallOutcome,
};
use crate::events::{EventId, RunEvent, RunEventPayload};
use crate::runtime::state::{Checkpoint, EventOutboxEntry};
use crate::types::{AgentResult, CompletionReason, ToolExecutionResult};

fn host_tool_result<'a>(
    snapshot: &'a Checkpoint,
    request: &HostInteractionRequest,
) -> CheckpointResult<&'a ToolExecutionResult> {
    let cycle = snapshot
        .cycles
        .iter()
        .find(|cycle| u64::from(cycle.index) == request.logical_cycle)
        .ok_or_else(|| {
            CheckpointError::new(
                "host_interaction_conflict",
                "host interaction cycle is missing",
            )
        })?;
    if !cycle
        .tool_calls
        .iter()
        .map(|call| &call.id)
        .eq(cycle.tool_results.iter().map(|result| &result.tool_call_id))
    {
        return Err(CheckpointError::new(
            "host_interaction_conflict",
            "host interaction cycle has incomplete tool results",
        ));
    }
    let result = cycle
        .tool_results
        .iter()
        .find(|result| result.tool_call_id == request.tool_call_id)
        .ok_or_else(|| {
            CheckpointError::new(
                "host_interaction_conflict",
                "host interaction result is missing from cycle",
            )
        })?;
    ToolCallOutcome::HostInteraction {
        result: result.clone(),
        request: request.clone(),
    }
    .validate()?;
    Ok(result)
}

pub(crate) fn validate_host_tool_receipt_replay(
    current: &Checkpoint,
    request: &HostInteractionRequest,
    context: &HostInteractionAdmissionContext,
) -> CheckpointResult<()> {
    if current.revision <= context.expected_revision {
        return Err(CheckpointError::new(
            "host_interaction_stale",
            "host interaction replay revision is stale",
        ));
    }
    if let Some(snapshot) = context.cycle_snapshot.as_deref() {
        if host_tool_result(current, request)? != host_tool_result(snapshot, request)? {
            return Err(CheckpointError::new(
                "host_interaction_conflict",
                "host interaction tool receipt conflicts",
            ));
        }
    }
    Ok(())
}

pub(crate) fn validate_host_recovery_cycle(
    current: &Checkpoint,
    request: &HostInteractionRequest,
) -> CheckpointResult<()> {
    if current.cycle_index.checked_add(1) == Some(request.logical_cycle) {
        return Ok(());
    }
    if current.cycle_index != request.logical_cycle {
        return Err(CheckpointError::new(
            "host_interaction_recovery_stale",
            "logical cycle does not match checkpoint",
        ));
    }
    host_tool_result(current, request)?;
    Ok(())
}

pub(crate) fn prepare_host_interaction_cycle(
    current: &Checkpoint,
    request: &HostInteractionRequest,
    context: &HostInteractionAdmissionContext,
) -> CheckpointResult<Checkpoint> {
    if current.cancel_requested {
        return Err(CheckpointError::new(
            "checkpoint_cancel_requested",
            "cancelled run cannot enter host interaction",
        ));
    }
    let Some(completed) = context.cycle_snapshot.as_deref() else {
        if !current.model_call_journal.is_empty() || !current.tool_journal.is_empty() {
            return Err(CheckpointError::new(
                "host_interaction_conflict",
                "host interaction requires the completed cycle",
            ));
        }
        return Ok(current.clone());
    };
    if !crate::runtime::state::checkpoint_definition_matches(current, completed)
        || completed.checkpoint_key != current.checkpoint_key
        || completed.revision != current.revision
        || completed.cycles.last().map(|cycle| u64::from(cycle.index))
            != Some(request.logical_cycle)
    {
        return Err(CheckpointError::new(
            "host_interaction_stale",
            "host interaction cycle snapshot is stale",
        ));
    }
    let result = host_tool_result(completed, request)?;
    let entry = current
        .tool_journal
        .iter()
        .find(|entry| {
            entry.operation_id == request.operation_id
                && entry.tool_call_id.as_deref() == Some(request.tool_call_id.as_str())
                && entry.cycle_index == request.logical_cycle
        })
        .ok_or_else(|| {
            CheckpointError::new(
                "host_interaction_conflict",
                "host interaction tool operation is missing",
            )
        })?;
    let mut updated = crate::runtime::state::prepare_tool_receipt(
        current,
        current,
        &entry.operation_id,
        entry.attempt,
        &request.tool_call_id,
        &entry.request_digest,
        result,
        &context.claim_token,
        context.expected_revision,
        context.claimed_cycle,
    )?
    .ok_or_else(|| {
        CheckpointError::new(
            "host_interaction_conflict",
            "host interaction tool receipt was not admitted",
        )
    })?;
    if updated
        .model_call_journal
        .iter()
        .chain(&updated.tool_journal)
        .any(|entry| {
            !matches!(
                entry.state,
                OperationState::Succeeded | OperationState::Failed
            )
        })
    {
        return Err(CheckpointError::new(
            "host_interaction_conflict",
            "host interaction cannot commit unresolved operations",
        ));
    }
    updated.messages = completed.messages.clone();
    updated.cycles = completed.cycles.clone();
    updated.shared_state = completed.shared_state.clone();
    updated.extension_state = completed.extension_state.clone();
    updated.budget_usage = completed.budget_usage.clone();
    updated.cycle_index = request.logical_cycle;
    updated.model_call_journal.clear();
    updated.tool_journal.clear();
    Ok(updated)
}

pub(crate) fn append_control_event(
    checkpoint: &mut Checkpoint,
    command_id: &str,
    payload: RunEventPayload,
) -> CheckpointResult<()> {
    append_control_event_with_completion(checkpoint, command_id, payload, None)
}

pub(crate) fn append_control_event_with_result(
    checkpoint: &mut Checkpoint,
    command_id: &str,
    payload: RunEventPayload,
    result: &AgentResult,
) -> CheckpointResult<()> {
    append_control_event_with_completion(checkpoint, command_id, payload, Some(result))
}

fn append_control_event_with_completion(
    checkpoint: &mut Checkpoint,
    command_id: &str,
    payload: RunEventPayload,
    result: Option<&AgentResult>,
) -> CheckpointResult<()> {
    // Controller events describe the checkpoint that was just committed.  A
    // control transition must never invent the next execution cycle; the
    // distributed worker owns that cycle claim and will emit its own events.
    let cycle_index = u32::try_from(checkpoint.cycle_index)
        .ok()
        .filter(|cycle| *cycle > 0);
    let event_kind = match &payload {
        RunEventPayload::RunStateChanged { .. } => "run_state_changed",
        RunEventPayload::RunCancelled { .. } => "run_cancelled",
        RunEventPayload::RunFailed { .. } => "run_failed",
        _ => "control",
    };
    let mut event = RunEvent::new(
        checkpoint.root_run_id.clone(),
        checkpoint.trace_id.clone(),
        "vv-agent",
        cycle_index,
        payload,
    );
    if let Some(result) = result {
        event = event
            .with_completion_details(
                result.completion_reason,
                result.completion_tool_name.as_deref(),
                result.partial_output.as_deref(),
            )
            .with_budget_details(
                result.budget_usage.as_ref(),
                result.budget_exhaustion.as_ref(),
            );
        if let Some(error_code) = result.error_code.as_deref() {
            event.metadata.insert(
                "error_code".to_string(),
                serde_json::Value::String(error_code.to_string()),
            );
        }
    }
    event.event_id = EventId::stable(format!("controller-{command_id}-{event_kind}"))
        .map_err(|error| CheckpointError::new("event_identity_conflict", error))?;
    let event_value = serde_json::to_value(&event)
        .map_err(|error| CheckpointError::new("checkpoint_event_invalid", error.to_string()))?;
    checkpoint.event_outbox.push(EventOutboxEntry::pending(
        event.event_id.as_str(),
        event_value,
    )?);
    Ok(())
}

pub(crate) fn append_cancel_requested_event(
    checkpoint: &mut Checkpoint,
    command_id: &str,
) -> CheckpointResult<()> {
    let cycle_index = u32::try_from(checkpoint.cycle_index)
        .ok()
        .filter(|cycle| *cycle > 0);
    let event = RunEvent::new(
        checkpoint.root_run_id.clone(),
        checkpoint.trace_id.clone(),
        "vv-agent",
        cycle_index,
        RunEventPayload::RunStateChanged {
            state: "running".to_string(),
        },
    )
    .with_cancel_requested_transition()
    .with_event_id(format!("controller-{command_id}-run_state_changed"))
    .map_err(|error| CheckpointError::new("event_identity_conflict", error))?;
    let event_value = serde_json::to_value(event)
        .map_err(|error| CheckpointError::new("checkpoint_event_invalid", error.to_string()))?;
    checkpoint.event_outbox.push(EventOutboxEntry::pending(
        format!("controller-{command_id}-run_state_changed"),
        event_value,
    )?);
    Ok(())
}

pub(crate) fn control_result(
    checkpoint: &Checkpoint,
    reason: CompletionReason,
    error: &str,
    code: Option<&str>,
) -> AgentResult {
    let mut result = AgentResult::failed_with_code(code.unwrap_or("agent_failed"), error, false);
    result.messages = checkpoint.messages.clone();
    result.cycles = checkpoint.cycles.clone();
    result.shared_state = checkpoint.shared_state.clone();
    result.budget_usage = checkpoint.budget_usage.clone();
    result.checkpoint_key = Some(checkpoint.checkpoint_key.clone());
    result.completion_reason = Some(reason);
    result.error_code = code.map(str::to_string);
    result.token_usage =
        crate::runtime::token_usage::summarize_task_token_usage(&checkpoint.model_calls);
    result
}
