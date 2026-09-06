use crate::checkpoint::{CheckpointError, CheckpointResult};
use crate::events::{EventId, RunEvent, RunEventPayload};
use crate::runtime::state::{Checkpoint, EventOutboxEntry};
use crate::types::{AgentResult, CompletionReason};

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
