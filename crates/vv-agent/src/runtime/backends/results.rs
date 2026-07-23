use crate::runtime::CancellationToken;
use crate::types::{
    last_assistant_output, AgentResult, AgentStatus, CompletionReason, CycleRecord, Message,
    Metadata, TaskTokenUsage,
};

pub(crate) fn execute_cycle_loop<F>(
    messages: Vec<Message>,
    shared_state: Metadata,
    cycle_executor: F,
    cancellation_token: Option<&CancellationToken>,
    max_cycles: u32,
) -> AgentResult
where
    F: FnMut(
        u32,
        &mut Vec<Message>,
        &mut Vec<CycleRecord>,
        &mut Metadata,
        Option<&CancellationToken>,
    ) -> Option<AgentResult>,
{
    execute_cycle_loop_with_state(
        messages,
        Vec::new(),
        shared_state,
        cycle_executor,
        cancellation_token,
        1,
        max_cycles,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_cycle_loop_with_state<F>(
    mut messages: Vec<Message>,
    mut cycles: Vec<CycleRecord>,
    mut shared_state: Metadata,
    mut cycle_executor: F,
    cancellation_token: Option<&CancellationToken>,
    cycle_index_start: u32,
    cycle_count: u32,
) -> AgentResult
where
    F: FnMut(
        u32,
        &mut Vec<Message>,
        &mut Vec<CycleRecord>,
        &mut Metadata,
        Option<&CancellationToken>,
    ) -> Option<AgentResult>,
{
    for offset in 0..cycle_count {
        let cycle_index = cycle_index_start.saturating_add(offset);
        if cancellation_token.is_some_and(CancellationToken::is_cancelled) {
            return cancelled_backend_result(messages, cycles, shared_state);
        }
        if let Some(result) = cycle_executor(
            cycle_index,
            &mut messages,
            &mut cycles,
            &mut shared_state,
            cancellation_token,
        ) {
            return result;
        }
    }

    let token_usage = TaskTokenUsage::default();
    let partial_output = last_assistant_output(&cycles);
    AgentResult {
        status: AgentStatus::MaxCycles,
        messages,
        cycles,
        completion_reason: Some(CompletionReason::MaxCycles),
        completion_tool_name: None,
        partial_output,
        budget_usage: None,
        budget_exhaustion: None,
        checkpoint_key: None,
        resume_observation: None,
        final_answer: Some("Reached max cycles without finish signal.".to_string()),
        wait_reason: None,
        error: None,
        error_code: None,
        shared_state,
        token_usage,
    }
}

pub(crate) fn cancelled_backend_result(
    messages: Vec<Message>,
    cycles: Vec<CycleRecord>,
    shared_state: Metadata,
) -> AgentResult {
    let token_usage = TaskTokenUsage::default();
    let partial_output = last_assistant_output(&cycles);
    AgentResult {
        status: AgentStatus::Failed,
        messages,
        cycles,
        completion_reason: Some(CompletionReason::Cancelled),
        completion_tool_name: None,
        partial_output,
        budget_usage: None,
        budget_exhaustion: None,
        checkpoint_key: None,
        resume_observation: None,
        final_answer: None,
        wait_reason: None,
        error: Some("Operation was cancelled".to_string()),
        error_code: None,
        shared_state,
        token_usage,
    }
}

pub(crate) fn failed_backend_result(
    messages: Vec<Message>,
    cycles: Vec<CycleRecord>,
    shared_state: Metadata,
    error: String,
) -> AgentResult {
    let token_usage = TaskTokenUsage::default();
    let partial_output = last_assistant_output(&cycles);
    AgentResult {
        status: AgentStatus::Failed,
        messages,
        cycles,
        completion_reason: Some(CompletionReason::Failed),
        completion_tool_name: None,
        partial_output,
        budget_usage: None,
        budget_exhaustion: None,
        checkpoint_key: None,
        resume_observation: None,
        final_answer: None,
        wait_reason: None,
        error: Some(error),
        error_code: None,
        shared_state,
        token_usage,
    }
}
