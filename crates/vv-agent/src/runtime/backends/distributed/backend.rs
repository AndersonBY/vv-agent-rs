use std::time::Duration;

use std::sync::Arc;

use super::super::RuntimeRecipe;
use super::capabilities::DistributedCapabilityRegistry;
use super::dispatch::CycleDispatcher;
use super::driver::CycleEnqueuer;
use super::{DEFAULT_CYCLE_NAME, DEFAULT_LEASE_DURATION_MS};
use crate::checkpoint::{
    ControllerCommand, ControllerCommandResolution, HostInteractionAdmissionContext,
    HostInteractionOutcome, HostInteractionRecoveryEnvelope, HostInteractionRecoveryResult,
};
use crate::runtime::CheckpointStore;

#[derive(Clone)]
pub struct DistributedBackend {
    pub(super) runtime_recipe: Option<RuntimeRecipe>,
    pub(super) cycle_dispatcher: Option<Arc<dyn CycleDispatcher>>,
    pub(super) capability_registry: Option<DistributedCapabilityRegistry>,
    pub(super) cycle_enqueuer: Option<Arc<dyn CycleEnqueuer>>,
    pub(super) cycle_name: String,
    pub(super) dispatch_timeout: Duration,
    pub(super) lease_duration_ms: u64,
    pub(super) controller_store: Option<Arc<dyn CheckpointStore>>,
    pub(super) host_interaction_context: Option<HostInteractionAdmissionContext>,
}

impl std::fmt::Debug for DistributedBackend {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DistributedBackend")
            .field("runtime_recipe", &self.runtime_recipe)
            .field("has_cycle_dispatcher", &self.cycle_dispatcher.is_some())
            .field(
                "has_capability_registry",
                &self.capability_registry.is_some(),
            )
            .field("has_cycle_enqueuer", &self.cycle_enqueuer.is_some())
            .field("cycle_name", &self.cycle_name)
            .field("dispatch_timeout", &self.dispatch_timeout)
            .field("lease_duration_ms", &self.lease_duration_ms)
            .field("has_controller_store", &self.controller_store.is_some())
            .field(
                "has_host_interaction_context",
                &self.host_interaction_context.is_some(),
            )
            .finish()
    }
}

impl DistributedBackend {
    pub fn inline_fallback() -> Self {
        Self {
            runtime_recipe: None,
            cycle_dispatcher: None,
            capability_registry: None,
            cycle_enqueuer: None,
            cycle_name: DEFAULT_CYCLE_NAME.to_string(),
            dispatch_timeout: Duration::from_secs(10 * 60),
            lease_duration_ms: DEFAULT_LEASE_DURATION_MS,
            controller_store: None,
            host_interaction_context: None,
        }
    }

    pub fn new(runtime_recipe: RuntimeRecipe, cycle_dispatcher: Arc<dyn CycleDispatcher>) -> Self {
        Self {
            runtime_recipe: Some(runtime_recipe),
            cycle_dispatcher: Some(cycle_dispatcher),
            capability_registry: None,
            cycle_enqueuer: None,
            cycle_name: DEFAULT_CYCLE_NAME.to_string(),
            dispatch_timeout: Duration::from_secs(10 * 60),
            lease_duration_ms: DEFAULT_LEASE_DURATION_MS,
            controller_store: None,
            host_interaction_context: None,
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

    pub fn cycle_name(&self) -> &str {
        &self.cycle_name
    }

    pub fn lease_duration_ms(&self) -> u64 {
        self.lease_duration_ms
    }

    /// Bind controller operations to the same durable authority used by the
    /// distributed worker. Without this binding the backend resolves the
    /// recipe's registered store and otherwise fails closed.
    pub fn with_controller_store(mut self, store: Arc<dyn CheckpointStore>) -> Self {
        self.controller_store = Some(store);
        self
    }

    /// Bind the worker's authoritative execution claim to framework-produced
    /// host interaction admission.  The public producer method still accepts
    /// only the canonical request; this context is trusted runtime state, not
    /// request wire data.
    pub fn with_host_interaction_context(
        mut self,
        context: HostInteractionAdmissionContext,
    ) -> Self {
        self.host_interaction_context = Some(context);
        self
    }

    fn controller_store(&self) -> Result<Arc<dyn CheckpointStore>, String> {
        if let Some(store) = &self.controller_store {
            return Ok(store.clone());
        }
        let recipe = self.runtime_recipe.as_ref().ok_or_else(|| {
            "distributed controller requires a bound checkpoint store".to_string()
        })?;
        let registry = self
            .capability_registry
            .as_ref()
            .ok_or_else(|| "distributed controller requires a capability registry".to_string())?;
        let reference = recipe
            .capabilities
            .checkpoint_store_ref
            .as_ref()
            .ok_or_else(|| "distributed controller requires checkpoint_store_ref".to_string())?;
        registry
            .resolve_checkpoint_store_required(reference)
            .map_err(|error| error.to_string())
    }

    pub fn produce_host_interaction(
        &self,
        request: crate::checkpoint::HostInteractionRequest,
    ) -> Result<HostInteractionOutcome, String> {
        let context = self.host_interaction_context.as_ref().ok_or_else(|| {
            "host interaction admission requires a bound execution claim context".to_string()
        })?;
        self.controller_store()?
            .produce_host_interaction(request, context)
            .map_err(|error| error.to_string())
    }

    pub fn resolve_controller_command(
        &self,
        command: ControllerCommand,
    ) -> Result<ControllerCommandResolution, String> {
        self.controller_store()?
            .resolve_controller_command(command)
            .map_err(|error| error.to_string())
    }

    pub fn claim_and_consume_host_interaction_response(
        &self,
        envelope: HostInteractionRecoveryEnvelope,
    ) -> Result<HostInteractionRecoveryResult, String> {
        self.controller_store()?
            .claim_and_consume_host_interaction_response(envelope)
            .map_err(|error| error.to_string())
    }

    pub(crate) fn has_nonblocking_driver(&self) -> bool {
        self.runtime_recipe.is_some()
            && self.capability_registry.is_some()
            && self.cycle_enqueuer.is_some()
    }

    pub fn parallel_map<T, R, F>(&self, function: F, items: Vec<T>) -> Vec<R>
    where
        F: Fn(T) -> R,
    {
        items.into_iter().map(function).collect()
    }
}
