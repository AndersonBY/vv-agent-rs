use super::*;

pub(super) fn cancellation_result(result: &AgentResult, checkpoint: &Checkpoint) -> AgentResult {
    AgentResult {
        status: AgentStatus::Failed,
        messages: result.messages.clone(),
        cycles: result.cycles.clone(),
        completion_reason: Some(crate::types::CompletionReason::Cancelled),
        completion_tool_name: None,
        partial_output: result
            .partial_output
            .clone()
            .or_else(|| last_assistant_output(&result.cycles)),
        budget_usage: result.budget_usage.clone(),
        budget_exhaustion: None,
        checkpoint_key: result.checkpoint_key.clone(),
        resume_observations: Vec::new(),
        final_answer: None,
        wait_reason: None,
        error: Some(crate::types::AgentResultError::new(
            "cancelled_with_unknown_outcome",
            "Cancellation was accepted while the external outcome remained unknown.",
            false,
        )),
        error_code: Some("cancelled_with_unknown_outcome".to_string()),
        shared_state: result.shared_state.clone(),
        token_usage: summarize_task_token_usage(&checkpoint.model_calls),
    }
}

pub(super) fn cancellation_event(event: RunEvent, result: &AgentResult) -> RunEvent {
    let mut replacement = RunEvent::new(
        event.run_id(),
        event.trace_id(),
        event.agent_name().unwrap_or_default(),
        event.cycle_index(),
        RunEventPayload::RunCancelled {
            reason: result
                .error
                .as_ref()
                .map(|error| error.message.clone())
                .unwrap_or_else(|| "run cancelled".to_string()),
        },
    )
    .with_completion_details(
        result.completion_reason,
        result.completion_tool_name.as_deref(),
        result.partial_output.as_deref(),
    )
    .with_budget_details(
        result.budget_usage.as_ref(),
        result.budget_exhaustion.as_ref(),
    );
    if let Some(session_id) = event.session_id() {
        replacement = replacement.with_session_id(session_id);
    }
    if let Some(parent_event_id) = event.parent_event_id() {
        replacement = replacement.with_parent_event_id(parent_event_id);
    }
    if let Some(parent_run_id) = event.parent_run_id() {
        replacement = replacement.with_parent_run_id(parent_run_id);
    }
    for (key, value) in event.metadata() {
        replacement = replacement.with_metadata(key.clone(), value.clone());
    }
    replacement
}
