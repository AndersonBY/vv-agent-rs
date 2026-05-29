use super::super::token_usage::summarize_task_token_usage;
use crate::runtime::CancellationToken;
use crate::types::{AgentResult, AgentStatus, CycleRecord, Message, Metadata};

pub(crate) fn execute_cycle_loop<F>(
    mut messages: Vec<Message>,
    mut shared_state: Metadata,
    mut cycle_executor: F,
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
    let mut cycles = Vec::new();

    for cycle_index in 1..=max_cycles {
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

    let token_usage = summarize_task_token_usage(&cycles);
    AgentResult {
        status: AgentStatus::MaxCycles,
        messages,
        cycles,
        final_answer: Some("Reached max cycles without finish signal.".to_string()),
        wait_reason: None,
        error: None,
        shared_state,
        token_usage,
    }
}

pub(crate) fn cancelled_backend_result(
    messages: Vec<Message>,
    cycles: Vec<CycleRecord>,
    shared_state: Metadata,
) -> AgentResult {
    let token_usage = summarize_task_token_usage(&cycles);
    AgentResult {
        status: AgentStatus::Failed,
        messages,
        cycles,
        final_answer: None,
        wait_reason: None,
        error: Some("Operation was cancelled".to_string()),
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
    let token_usage = summarize_task_token_usage(&cycles);
    AgentResult {
        status: AgentStatus::Failed,
        messages,
        cycles,
        final_answer: None,
        wait_reason: None,
        error: Some(error),
        shared_state,
        token_usage,
    }
}
