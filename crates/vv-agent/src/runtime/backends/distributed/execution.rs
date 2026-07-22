use crate::budget::{BudgetUsageSnapshot, RunBudgetLimits};
use crate::runtime::checkpoint_resume::CheckpointController;
use crate::runtime::CancellationToken;
use crate::types::{AgentResult, AgentTask, CycleRecord, Message, Metadata};

use super::super::{execute_cycle_loop, execute_cycle_loop_with_state, failed_backend_result};
use super::backend::DistributedBackend;

impl DistributedBackend {
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
        if self.runtime_recipe.is_some() {
            return failed_backend_result(
                initial_messages,
                Vec::new(),
                shared_state,
                "DistributedBackend requires RunConfig.checkpoint_config".to_string(),
            );
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
        budget_limits: Option<RunBudgetLimits>,
        _initial_budget_usage: Option<BudgetUsageSnapshot>,
        checkpoint_controller: Option<CheckpointController>,
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
        if self.runtime_recipe.is_some()
            && checkpoint_controller.is_none()
            && (!initial_cycles.is_empty() || cycle_index_start != 1)
        {
            return failed_backend_result(
                initial_messages,
                initial_cycles,
                shared_state,
                "Checkpoint resume cannot recursively dispatch a distributed backend".to_string(),
            );
        }
        if self.runtime_recipe.is_some() {
            if let Some(checkpoint_controller) = checkpoint_controller {
                return self.execute_distributed(
                    task,
                    cycle_index_start,
                    cycle_count,
                    budget_limits,
                    cancellation_token,
                    checkpoint_controller,
                );
            }
            return failed_backend_result(
                initial_messages,
                initial_cycles,
                shared_state,
                "DistributedBackend requires RunConfig.checkpoint_config".to_string(),
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
