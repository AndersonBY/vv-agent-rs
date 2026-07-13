use crate::runtime::CancellationToken;
use crate::types::{AgentResult, AgentTask, CycleRecord, Message, Metadata};

use super::super::{execute_cycle_loop, execute_cycle_loop_with_state, failed_backend_result};
use super::backend::DistributedBackend;
use super::r#loop::DistributedRunContext;

impl DistributedBackend {
    pub fn execute<F>(
        &self,
        task: &AgentTask,
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
        match (
            &self.runtime_recipe,
            &self.state_store,
            &self.cycle_dispatcher,
        ) {
            (Some(recipe), Some(state_store), Some(cycle_dispatcher)) => {
                return self.execute_distributed(
                    initial_messages,
                    shared_state,
                    DistributedRunContext {
                        task,
                        recipe,
                        state_store,
                        cycle_dispatcher,
                        cancellation_token,
                        max_cycles,
                    },
                );
            }
            (Some(_), _, _) => {
                return failed_backend_result(
                    initial_messages,
                    Vec::new(),
                    shared_state,
                    "Distributed backend requires a state_store and cycle_dispatcher".to_string(),
                );
            }
            (None, _, _) => {}
        }
        execute_cycle_loop(
            initial_messages,
            shared_state,
            cycle_executor,
            cancellation_token,
            max_cycles,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn execute_with_state<F>(
        &self,
        task: &AgentTask,
        initial_messages: Vec<Message>,
        initial_cycles: Vec<CycleRecord>,
        shared_state: Metadata,
        cycle_executor: F,
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
        if self.runtime_recipe.is_some() && (!initial_cycles.is_empty() || cycle_index_start != 1) {
            return failed_backend_result(
                initial_messages,
                initial_cycles,
                shared_state,
                "Checkpoint resume cannot recursively dispatch a distributed backend".to_string(),
            );
        }
        if self.runtime_recipe.is_some() {
            return self.execute(
                task,
                initial_messages,
                shared_state,
                cycle_executor,
                cancellation_token,
                cycle_count,
            );
        }
        execute_cycle_loop_with_state(
            initial_messages,
            initial_cycles,
            shared_state,
            cycle_executor,
            cancellation_token,
            cycle_index_start,
            cycle_count,
        )
    }
}
