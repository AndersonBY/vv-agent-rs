use std::sync::Arc;
use std::time::Duration;

use crate::runtime::state::StateStore;

use super::super::RuntimeRecipe;
use super::dispatch::CycleDispatcher;
use super::{DEFAULT_CYCLE_NAME, DEFAULT_LEASE_DURATION_MS};

#[derive(Clone)]
pub struct DistributedBackend {
    pub(super) runtime_recipe: Option<RuntimeRecipe>,
    pub(super) state_store: Option<Arc<dyn StateStore>>,
    pub(super) cycle_dispatcher: Option<Arc<dyn CycleDispatcher>>,
    pub(super) cycle_name: String,
    pub(super) dispatch_timeout: Duration,
    pub(super) lease_duration_ms: u64,
}

impl std::fmt::Debug for DistributedBackend {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DistributedBackend")
            .field("runtime_recipe", &self.runtime_recipe)
            .field("has_state_store", &self.state_store.is_some())
            .field("has_cycle_dispatcher", &self.cycle_dispatcher.is_some())
            .field("cycle_name", &self.cycle_name)
            .field("dispatch_timeout", &self.dispatch_timeout)
            .field("lease_duration_ms", &self.lease_duration_ms)
            .finish()
    }
}

impl DistributedBackend {
    pub fn inline_fallback() -> Self {
        Self {
            runtime_recipe: None,
            state_store: None,
            cycle_dispatcher: None,
            cycle_name: DEFAULT_CYCLE_NAME.to_string(),
            dispatch_timeout: Duration::from_secs(10 * 60),
            lease_duration_ms: DEFAULT_LEASE_DURATION_MS,
        }
    }

    pub fn distributed(runtime_recipe: RuntimeRecipe) -> Self {
        Self {
            runtime_recipe: Some(runtime_recipe),
            state_store: None,
            cycle_dispatcher: None,
            cycle_name: DEFAULT_CYCLE_NAME.to_string(),
            dispatch_timeout: Duration::from_secs(10 * 60),
            lease_duration_ms: DEFAULT_LEASE_DURATION_MS,
        }
    }

    pub fn distributed_with_dispatcher(
        runtime_recipe: RuntimeRecipe,
        state_store: Arc<dyn StateStore>,
        cycle_dispatcher: Arc<dyn CycleDispatcher>,
    ) -> Self {
        Self {
            runtime_recipe: Some(runtime_recipe),
            state_store: Some(state_store),
            cycle_dispatcher: Some(cycle_dispatcher),
            cycle_name: DEFAULT_CYCLE_NAME.to_string(),
            dispatch_timeout: Duration::from_secs(10 * 60),
            lease_duration_ms: DEFAULT_LEASE_DURATION_MS,
        }
    }

    pub fn with_cycle_name(mut self, cycle_name: impl Into<String>) -> Self {
        self.cycle_name = cycle_name.into();
        self
    }

    pub fn with_dispatch_timeout(mut self, timeout: Duration) -> Self {
        assert!(!timeout.is_zero(), "dispatch timeout must be positive");
        self.dispatch_timeout = timeout;
        self
    }

    pub fn with_lease_duration(mut self, duration: Duration) -> Self {
        let duration_ms = u64::try_from(duration.as_millis())
            .expect("lease duration milliseconds must fit in u64");
        assert!(duration_ms > 0, "lease duration must be positive");
        self.lease_duration_ms = duration_ms;
        self
    }

    pub fn runtime_recipe(&self) -> Option<&RuntimeRecipe> {
        self.runtime_recipe.as_ref()
    }

    pub fn state_store(&self) -> Option<&Arc<dyn StateStore>> {
        self.state_store.as_ref()
    }

    pub fn cycle_name(&self) -> &str {
        &self.cycle_name
    }

    pub fn parallel_map<T, R, F>(&self, function: F, items: Vec<T>) -> Vec<R>
    where
        F: Fn(T) -> R,
    {
        items.into_iter().map(function).collect()
    }
}
