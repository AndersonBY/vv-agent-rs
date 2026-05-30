use std::sync::Arc;

use crate::runtime::state::StateStore;

use super::super::RuntimeRecipe;
use super::dispatch::CycleTaskDispatcher;

const DEFAULT_CYCLE_TASK_NAME: &str = "vv_agent.celery_tasks.run_single_cycle";

#[derive(Clone)]
pub struct CeleryBackend {
    pub(super) runtime_recipe: Option<RuntimeRecipe>,
    pub(super) state_store: Option<Arc<dyn StateStore>>,
    pub(super) cycle_dispatcher: Option<Arc<dyn CycleTaskDispatcher>>,
    pub(super) cycle_task_name: String,
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
            cycle_task_name: DEFAULT_CYCLE_TASK_NAME.to_string(),
        }
    }

    pub fn distributed(runtime_recipe: RuntimeRecipe) -> Self {
        Self {
            runtime_recipe: Some(runtime_recipe),
            state_store: None,
            cycle_dispatcher: None,
            cycle_task_name: DEFAULT_CYCLE_TASK_NAME.to_string(),
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
            cycle_task_name: DEFAULT_CYCLE_TASK_NAME.to_string(),
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

    pub fn parallel_map<T, R, F>(&self, function: F, items: Vec<T>) -> Vec<R>
    where
        F: Fn(T) -> R,
    {
        items.into_iter().map(function).collect()
    }
}
