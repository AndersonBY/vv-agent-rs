use std::sync::Arc;

use crate::runtime::state::{Checkpoint, StateStore};
use crate::runtime::token_usage::summarize_task_token_usage;
use crate::runtime::CancellationToken;
use crate::types::{AgentResult, AgentStatus, AgentTask, Message, Metadata};

use super::super::{cancelled_backend_result, failed_backend_result, RuntimeRecipe};
use super::backend::DistributedBackend;
use super::checkpoint::checkpoint_snapshot;
use super::dispatch::CycleDispatcher;

pub(super) struct DistributedRunContext<'a> {
    pub task: &'a AgentTask,
    pub recipe: &'a RuntimeRecipe,
    pub state_store: &'a Arc<dyn StateStore>,
    pub cycle_dispatcher: &'a Arc<dyn CycleDispatcher>,
    pub cancellation_token: Option<&'a CancellationToken>,
    pub max_cycles: u32,
}

impl DistributedBackend {
    pub(super) fn execute_distributed(
        &self,
        initial_messages: Vec<Message>,
        shared_state: Metadata,
        context: DistributedRunContext<'_>,
    ) -> AgentResult {
        let checkpoint = Checkpoint {
            task_id: context.task.task_id.clone(),
            cycle_index: 0,
            status: AgentStatus::Running,
            messages: initial_messages.clone(),
            cycles: Vec::new(),
            shared_state: shared_state.clone(),
        };
        if let Err(error) = context.state_store.save_checkpoint(checkpoint) {
            return AgentResult {
                status: AgentStatus::Failed,
                messages: initial_messages,
                cycles: Vec::new(),
                final_answer: None,
                wait_reason: None,
                error: Some(format!("Failed to save initial checkpoint: {error}")),
                shared_state,
                token_usage: Default::default(),
            };
        }

        let result = self.distributed_loop(&context);
        let _ = context.state_store.delete_checkpoint(&context.task.task_id);
        result
    }

    fn distributed_loop(&self, context: &DistributedRunContext<'_>) -> AgentResult {
        for cycle_index in 1..=context.max_cycles {
            if context
                .cancellation_token
                .is_some_and(CancellationToken::is_cancelled)
            {
                let (messages, cycles, shared_state) =
                    checkpoint_snapshot(context.state_store, &context.task.task_id);
                return cancelled_backend_result(messages, cycles, shared_state);
            }

            match context.cycle_dispatcher.dispatch_cycle(
                context.task,
                context.recipe,
                &self.cycle_name,
                cycle_index,
            ) {
                Ok(dispatch_result) if dispatch_result.finished => {
                    return dispatch_result.result.unwrap_or_else(|| {
                        let (messages, cycles, shared_state) =
                            checkpoint_snapshot(context.state_store, &context.task.task_id);
                        failed_backend_result(
                            messages,
                            cycles,
                            shared_state,
                            format!(
                                "Distributed cycle {cycle_index} finished without result payload"
                            ),
                        )
                    });
                }
                Ok(_) => {}
                Err(error) => {
                    let (messages, cycles, shared_state) =
                        checkpoint_snapshot(context.state_store, &context.task.task_id);
                    return failed_backend_result(
                        messages,
                        cycles,
                        shared_state,
                        format!("Distributed cycle {cycle_index} failed: {error}"),
                    );
                }
            }
        }

        let (messages, cycles, shared_state) =
            checkpoint_snapshot(context.state_store, &context.task.task_id);
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
}
