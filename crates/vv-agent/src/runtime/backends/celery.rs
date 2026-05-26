use std::sync::Arc;

use crate::runtime::state::{Checkpoint, StateStore};
use crate::runtime::token_usage::summarize_task_token_usage;
use crate::runtime::CancellationToken;
use crate::types::{AgentResult, AgentStatus, AgentTask, CycleRecord, Message, Metadata};

use super::{cancelled_backend_result, execute_cycle_loop, failed_backend_result, RuntimeRecipe};

#[derive(Debug, Clone, PartialEq)]
pub struct CycleTaskDispatchResult {
    pub finished: bool,
    pub result: Option<AgentResult>,
}

impl CycleTaskDispatchResult {
    pub fn unfinished() -> Self {
        Self {
            finished: false,
            result: None,
        }
    }

    pub fn finished(result: AgentResult) -> Self {
        Self {
            finished: true,
            result: Some(result),
        }
    }
}

pub trait CycleTaskDispatcher: Send + Sync {
    fn dispatch_cycle(
        &self,
        task: &AgentTask,
        recipe: &RuntimeRecipe,
        cycle_task_name: &str,
        cycle_index: u32,
    ) -> Result<CycleTaskDispatchResult, String>;
}

#[derive(Clone)]
pub struct CeleryBackend {
    runtime_recipe: Option<RuntimeRecipe>,
    state_store: Option<Arc<dyn StateStore>>,
    cycle_dispatcher: Option<Arc<dyn CycleTaskDispatcher>>,
    cycle_task_name: String,
}

struct DistributedRunContext<'a> {
    task: &'a AgentTask,
    recipe: &'a RuntimeRecipe,
    state_store: &'a Arc<dyn StateStore>,
    cycle_dispatcher: &'a Arc<dyn CycleTaskDispatcher>,
    cancellation_token: Option<&'a CancellationToken>,
    max_cycles: u32,
}

impl std::fmt::Debug for CeleryBackend {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CeleryBackend")
            .field("runtime_recipe", &self.runtime_recipe)
            .field("has_state_store", &self.state_store.is_some())
            .field("has_cycle_dispatcher", &self.cycle_dispatcher.is_some())
            .field("cycle_task_name", &self.cycle_task_name)
            .finish()
    }
}

impl CeleryBackend {
    pub fn inline_fallback() -> Self {
        Self {
            runtime_recipe: None,
            state_store: None,
            cycle_dispatcher: None,
            cycle_task_name: "vv_agent.celery_tasks.run_single_cycle".to_string(),
        }
    }

    pub fn distributed(runtime_recipe: RuntimeRecipe) -> Self {
        Self {
            runtime_recipe: Some(runtime_recipe),
            state_store: None,
            cycle_dispatcher: None,
            cycle_task_name: "vv_agent.celery_tasks.run_single_cycle".to_string(),
        }
    }

    pub fn distributed_with_dispatcher(
        runtime_recipe: RuntimeRecipe,
        state_store: Arc<dyn StateStore>,
        cycle_dispatcher: Arc<dyn CycleTaskDispatcher>,
    ) -> Self {
        Self {
            runtime_recipe: Some(runtime_recipe),
            state_store: Some(state_store),
            cycle_dispatcher: Some(cycle_dispatcher),
            cycle_task_name: "vv_agent.celery_tasks.run_single_cycle".to_string(),
        }
    }

    pub fn with_cycle_task_name(mut self, cycle_task_name: impl Into<String>) -> Self {
        self.cycle_task_name = cycle_task_name.into();
        self
    }

    pub fn runtime_recipe(&self) -> Option<&RuntimeRecipe> {
        self.runtime_recipe.as_ref()
    }

    pub fn state_store(&self) -> Option<&Arc<dyn StateStore>> {
        self.state_store.as_ref()
    }

    pub fn cycle_task_name(&self) -> &str {
        &self.cycle_task_name
    }

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
        if let (Some(recipe), Some(state_store), Some(cycle_dispatcher)) = (
            &self.runtime_recipe,
            &self.state_store,
            &self.cycle_dispatcher,
        ) {
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
        execute_cycle_loop(
            initial_messages,
            shared_state,
            cycle_executor,
            cancellation_token,
            max_cycles,
        )
    }

    fn execute_distributed(
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
                &self.cycle_task_name,
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
                            format!("Celery cycle {cycle_index} finished without result payload"),
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
                        format!("Celery cycle {cycle_index} failed: {error}"),
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

    pub fn parallel_map<T, R, F>(&self, function: F, items: Vec<T>) -> Vec<R>
    where
        F: Fn(T) -> R,
    {
        items.into_iter().map(function).collect()
    }
}

fn checkpoint_snapshot(
    state_store: &Arc<dyn StateStore>,
    task_id: &str,
) -> (Vec<Message>, Vec<CycleRecord>, Metadata) {
    match state_store.load_checkpoint(task_id) {
        Ok(Some(checkpoint)) => (
            checkpoint.messages,
            checkpoint.cycles,
            checkpoint.shared_state,
        ),
        Ok(None) | Err(_) => (Vec::new(), Vec::new(), Metadata::new()),
    }
}
