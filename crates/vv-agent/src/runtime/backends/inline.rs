use super::execute_cycle_loop;
use crate::runtime::CancellationToken;
use crate::types::{AgentResult, AgentTask, CycleRecord, Message, Metadata};

#[derive(Debug, Clone, Copy, Default)]
pub struct InlineBackend;

impl InlineBackend {
    pub fn execute<F>(
        &self,
        _task: &AgentTask,
        initial_messages: Vec<Message>,
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
        execute_cycle_loop(
            initial_messages,
            shared_state,
            cycle_executor,
            cancellation_token,
            max_cycles,
        )
    }

    pub fn parallel_map<T, R, F>(&self, function: F, items: Vec<T>) -> Vec<R>
    where
        F: Fn(T) -> R,
    {
        items.into_iter().map(function).collect()
    }
}
