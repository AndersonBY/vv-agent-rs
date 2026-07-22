use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use sha2::{Digest, Sha256};

use crate::approval::{ApprovalBroker, ApprovalProvider};
use crate::budget::HostCostMeter;
use crate::checkpoint::{CheckpointExtension, IdempotentRunEventStore, ReconciliationProvider};
use crate::llm::LlmClient;
use crate::memory::MemoryProvider;
use crate::runtime::engine::RunEventHandler;
use crate::runtime::hooks::RuntimeHook;
use crate::runtime::lifecycle::AfterCycleHook;
use crate::runtime::state::CheckpointStore;
use crate::runtime::sub_task_manager::SubTaskManager;
use crate::runtime::CancellationToken;
use crate::tools::{ApprovalPolicy, CanUseToolPredicate, ToolPolicy, ToolRegistry};
use crate::workspace::WorkspaceBackend;

use super::{
    CapabilityRef, DistributedCapabilities, DistributedCheckpointExtensionRef,
    DistributedToolPolicy, ToolsetRef,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DistributedCapabilityError {
    message: String,
}

impl DistributedCapabilityError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for DistributedCapabilityError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for DistributedCapabilityError {}

type CapabilityKey = (String, String);

#[derive(Default)]
struct CapabilityMaps {
    toolsets: BTreeMap<CapabilityKey, ToolRegistry>,
    llm_clients: BTreeMap<CapabilityKey, Arc<dyn LlmClient>>,
    workspace_backends: BTreeMap<CapabilityKey, Arc<dyn WorkspaceBackend>>,
    approval_providers: BTreeMap<CapabilityKey, Arc<dyn ApprovalProvider>>,
    approval_brokers: BTreeMap<CapabilityKey, ApprovalBroker>,
    cancellations: BTreeMap<CapabilityKey, CancellationToken>,
    event_sinks: BTreeMap<CapabilityKey, RunEventHandler>,
    host_cost_meters: BTreeMap<CapabilityKey, Arc<dyn HostCostMeter>>,
    app_states: BTreeMap<CapabilityKey, Arc<dyn std::any::Any + Send + Sync>>,
    memory_providers: BTreeMap<CapabilityKey, Arc<dyn MemoryProvider>>,
    hooks: BTreeMap<CapabilityKey, Arc<dyn RuntimeHook>>,
    after_cycle_hooks: BTreeMap<CapabilityKey, Arc<dyn AfterCycleHook>>,
    observers: BTreeMap<CapabilityKey, RunEventHandler>,
    sub_task_managers: BTreeMap<CapabilityKey, SubTaskManager>,
    tool_predicates: BTreeMap<CapabilityKey, CanUseToolPredicate>,
    checkpoint_stores: BTreeMap<CapabilityKey, Arc<dyn CheckpointStore>>,
    checkpoint_event_stores: BTreeMap<CapabilityKey, Arc<dyn IdempotentRunEventStore>>,
    checkpoint_extensions: BTreeMap<CapabilityKey, Arc<dyn CheckpointExtension>>,
    reconciliation_providers: BTreeMap<CapabilityKey, Arc<dyn ReconciliationProvider>>,
}

#[derive(Clone)]
pub struct DistributedCapabilityRegistry {
    inner: Arc<RwLock<CapabilityMaps>>,
}

impl Default for DistributedCapabilityRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl DistributedCapabilityRegistry {
    pub fn new() -> Self {
        let registry = Self::empty();
        registry
            .register_toolset(
                ToolsetRef::default(),
                crate::tools::build_default_registry(),
            )
            .expect("built-in tool schema digest is a compile-time parity contract");
        registry
    }

    pub fn empty() -> Self {
        Self {
            inner: Arc::new(RwLock::new(CapabilityMaps::default())),
        }
    }

    pub fn register_toolset(
        &self,
        reference: ToolsetRef,
        registry: ToolRegistry,
    ) -> Result<(), DistributedCapabilityError> {
        let actual = toolset_schema_digest(&registry)?;
        if actual != reference.schema_digest {
            return Err(DistributedCapabilityError::new(format!(
                "toolset {}@{} schema digest mismatch: expected {}, got {actual}",
                reference.id, reference.version, reference.schema_digest
            )));
        }
        self.write()?
            .toolsets
            .insert(key(&reference.capability_ref()), registry);
        Ok(())
    }

    pub fn register_llm_client(&self, reference: CapabilityRef, client: Arc<dyn LlmClient>) {
        self.write_unpoisoned()
            .llm_clients
            .insert(key(&reference), client);
    }

    pub fn register_workspace_backend(
        &self,
        reference: CapabilityRef,
        backend: Arc<dyn WorkspaceBackend>,
    ) {
        self.write_unpoisoned()
            .workspace_backends
            .insert(key(&reference), backend);
    }

    pub fn register_approval_provider(
        &self,
        reference: CapabilityRef,
        provider: Arc<dyn ApprovalProvider>,
    ) {
        self.write_unpoisoned()
            .approval_providers
            .insert(key(&reference), provider);
    }

    pub fn register_approval_broker(&self, reference: CapabilityRef, broker: ApprovalBroker) {
        self.write_unpoisoned()
            .approval_brokers
            .insert(key(&reference), broker);
    }

    pub fn register_cancellation(&self, reference: CapabilityRef, token: CancellationToken) {
        self.write_unpoisoned()
            .cancellations
            .insert(key(&reference), token);
    }

    pub fn register_event_sink(&self, reference: CapabilityRef, sink: RunEventHandler) {
        self.write_unpoisoned()
            .event_sinks
            .insert(key(&reference), sink);
    }

    pub fn register_host_cost_meter(
        &self,
        reference: CapabilityRef,
        meter: Arc<dyn HostCostMeter>,
    ) {
        self.write_unpoisoned()
            .host_cost_meters
            .insert(key(&reference), meter);
    }

    pub fn register_app_state(
        &self,
        reference: CapabilityRef,
        state: Arc<dyn std::any::Any + Send + Sync>,
    ) {
        self.write_unpoisoned()
            .app_states
            .insert(key(&reference), state);
    }

    pub fn register_memory_provider(
        &self,
        reference: CapabilityRef,
        provider: Arc<dyn MemoryProvider>,
    ) {
        self.write_unpoisoned()
            .memory_providers
            .insert(key(&reference), provider);
    }

    pub fn register_hook(&self, reference: CapabilityRef, hook: Arc<dyn RuntimeHook>) {
        self.write_unpoisoned().hooks.insert(key(&reference), hook);
    }

    pub fn register_after_cycle_hook(
        &self,
        reference: CapabilityRef,
        hook: Arc<dyn AfterCycleHook>,
    ) {
        self.write_unpoisoned()
            .after_cycle_hooks
            .insert(key(&reference), hook);
    }

    pub fn register_observer(&self, reference: CapabilityRef, observer: RunEventHandler) {
        self.write_unpoisoned()
            .observers
            .insert(key(&reference), observer);
    }

    pub fn register_sub_task_manager(&self, reference: CapabilityRef, manager: SubTaskManager) {
        self.write_unpoisoned()
            .sub_task_managers
            .insert(key(&reference), manager);
    }

    pub fn register_tool_predicate(
        &self,
        reference: CapabilityRef,
        predicate: CanUseToolPredicate,
    ) {
        self.write_unpoisoned()
            .tool_predicates
            .insert(key(&reference), predicate);
    }

    pub fn register_checkpoint_store(
        &self,
        reference: CapabilityRef,
        store: Arc<dyn CheckpointStore>,
    ) {
        self.write_unpoisoned()
            .checkpoint_stores
            .insert(key(&reference), store);
    }

    pub fn register_checkpoint_event_store(
        &self,
        reference: CapabilityRef,
        store: Arc<dyn IdempotentRunEventStore>,
    ) {
        self.write_unpoisoned()
            .checkpoint_event_stores
            .insert(key(&reference), store);
    }

    pub fn register_checkpoint_extension(
        &self,
        reference: CapabilityRef,
        extension: Arc<dyn CheckpointExtension>,
    ) {
        self.write_unpoisoned()
            .checkpoint_extensions
            .insert(key(&reference), extension);
    }

    pub fn register_reconciliation_provider(
        &self,
        reference: CapabilityRef,
        provider: Arc<dyn ReconciliationProvider>,
    ) {
        self.write_unpoisoned()
            .reconciliation_providers
            .insert(key(&reference), provider);
    }

    pub(crate) fn resolve_checkpoint_store_required(
        &self,
        reference: &CapabilityRef,
    ) -> Result<Arc<dyn CheckpointStore>, DistributedCapabilityError> {
        self.read()?
            .checkpoint_stores
            .get(&key(reference))
            .cloned()
            .ok_or_else(|| unknown("checkpoint_store", reference))
    }

    pub fn resolve(
        &self,
        capabilities: &DistributedCapabilities,
    ) -> Result<ResolvedDistributedCapabilities, DistributedCapabilityError> {
        capabilities
            .validate()
            .map_err(DistributedCapabilityError::new)?;
        let maps = self.read()?;
        let tool_registry = resolve_toolset(&maps, &capabilities.toolset_ref)?;
        let tool_policy = resolve_tool_policy(&maps, &capabilities.tool_policy)?;
        let llm_client = optional(
            &maps.llm_clients,
            "llm_client",
            &capabilities.llm_client_ref,
        )?;
        let workspace_backend = optional(
            &maps.workspace_backends,
            "workspace_backend",
            &capabilities.workspace_backend_ref,
        )?;
        let approval_provider = optional(
            &maps.approval_providers,
            "approval_provider",
            &capabilities.approval_provider_ref,
        )?;
        let approval_broker = optional(
            &maps.approval_brokers,
            "approval_broker",
            &capabilities.approval_broker_ref,
        )?;
        let cancellation = optional(
            &maps.cancellations,
            "cancellation",
            &capabilities.cancellation_ref,
        )?;
        let event_sink = optional(
            &maps.event_sinks,
            "event_sink",
            &capabilities.event_sink_ref,
        )?;
        let host_cost_meter = optional(
            &maps.host_cost_meters,
            "host_cost_meter",
            &capabilities.host_cost_meter_ref,
        )?;
        let app_state = optional(&maps.app_states, "app_state", &capabilities.app_state_ref)?;
        let sub_task_manager = optional(
            &maps.sub_task_managers,
            "sub_task_manager",
            &capabilities.sub_task_manager_ref,
        )?;
        let memory_providers = required_many(
            &maps.memory_providers,
            "memory_provider",
            &capabilities.memory_provider_refs,
        )?;
        let hooks = required_many(&maps.hooks, "hook", &capabilities.hook_refs)?;
        let after_cycle_hooks = required_many(
            &maps.after_cycle_hooks,
            "after_cycle_hook",
            &capabilities.after_cycle_hook_refs,
        )?;
        let observers = required_many(&maps.observers, "observer", &capabilities.observer_refs)?;
        let checkpoint_store = optional(
            &maps.checkpoint_stores,
            "checkpoint_store",
            &capabilities.checkpoint_store_ref,
        )?;
        let checkpoint_event_store = optional(
            &maps.checkpoint_event_stores,
            "checkpoint_event_store",
            &capabilities.checkpoint_event_store_ref,
        )?;
        let checkpoint_extensions = capabilities
            .checkpoint_extension_refs
            .iter()
            .map(|descriptor| resolve_checkpoint_extension(&maps, descriptor))
            .collect::<Result<Vec<_>, _>>()?;
        let reconciliation_provider = optional(
            &maps.reconciliation_providers,
            "reconciliation_provider",
            &capabilities.reconciliation_provider_ref,
        )?;
        Ok(ResolvedDistributedCapabilities {
            tool_registry,
            tool_policy,
            llm_client,
            workspace_backend,
            approval_provider,
            approval_broker,
            approval_timeout_seconds: capabilities.approval_timeout_seconds,
            cancellation,
            event_sink,
            host_cost_meter,
            app_state,
            memory_providers,
            hooks,
            after_cycle_hooks,
            observers,
            sub_task_manager,
            checkpoint_store,
            checkpoint_event_store,
            checkpoint_extensions,
            reconciliation_provider,
        })
    }

    fn read(
        &self,
    ) -> Result<std::sync::RwLockReadGuard<'_, CapabilityMaps>, DistributedCapabilityError> {
        self.inner.read().map_err(|_| {
            DistributedCapabilityError::new("distributed capability registry lock poisoned")
        })
    }

    fn write(
        &self,
    ) -> Result<std::sync::RwLockWriteGuard<'_, CapabilityMaps>, DistributedCapabilityError> {
        self.inner.write().map_err(|_| {
            DistributedCapabilityError::new("distributed capability registry lock poisoned")
        })
    }

    fn write_unpoisoned(&self) -> std::sync::RwLockWriteGuard<'_, CapabilityMaps> {
        self.inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[derive(Clone)]
pub struct ResolvedDistributedCapabilities {
    pub tool_registry: ToolRegistry,
    pub tool_policy: ToolPolicy,
    pub llm_client: Option<Arc<dyn LlmClient>>,
    pub workspace_backend: Option<Arc<dyn WorkspaceBackend>>,
    pub approval_provider: Option<Arc<dyn ApprovalProvider>>,
    pub approval_broker: Option<ApprovalBroker>,
    pub approval_timeout_seconds: Option<f64>,
    pub cancellation: Option<CancellationToken>,
    pub event_sink: Option<RunEventHandler>,
    pub host_cost_meter: Option<Arc<dyn HostCostMeter>>,
    pub app_state: Option<Arc<dyn std::any::Any + Send + Sync>>,
    pub memory_providers: Vec<Arc<dyn MemoryProvider>>,
    pub hooks: Vec<Arc<dyn RuntimeHook>>,
    pub after_cycle_hooks: Vec<Arc<dyn AfterCycleHook>>,
    pub observers: Vec<RunEventHandler>,
    pub sub_task_manager: Option<SubTaskManager>,
    pub checkpoint_store: Option<Arc<dyn CheckpointStore>>,
    pub checkpoint_event_store: Option<Arc<dyn IdempotentRunEventStore>>,
    pub checkpoint_extensions: Vec<ResolvedDistributedCheckpointExtension>,
    pub reconciliation_provider: Option<Arc<dyn ReconciliationProvider>>,
}

#[derive(Clone)]
pub struct ResolvedDistributedCheckpointExtension {
    pub descriptor: DistributedCheckpointExtensionRef,
    pub extension: Arc<dyn CheckpointExtension>,
}

pub fn toolset_schema_digest(
    registry: &ToolRegistry,
) -> Result<String, DistributedCapabilityError> {
    let schemas = registry
        .list_openai_schemas(None)
        .map_err(DistributedCapabilityError::new)?;
    let canonical = serde_json::to_vec(&schemas).map_err(|error| {
        DistributedCapabilityError::new(format!("failed to serialize toolset schemas: {error}"))
    })?;
    Ok(format!("{:x}", Sha256::digest(canonical)))
}

fn resolve_toolset(
    maps: &CapabilityMaps,
    reference: &ToolsetRef,
) -> Result<ToolRegistry, DistributedCapabilityError> {
    let registry = maps
        .toolsets
        .get(&key(&reference.capability_ref()))
        .cloned()
        .ok_or_else(|| unknown("toolset", &reference.capability_ref()))?;
    let actual = toolset_schema_digest(&registry)?;
    if actual != reference.schema_digest {
        return Err(DistributedCapabilityError::new(format!(
            "toolset {}@{} schema digest mismatch: expected {}, got {actual}",
            reference.id, reference.version, reference.schema_digest
        )));
    }
    Ok(registry)
}

fn resolve_tool_policy(
    maps: &CapabilityMaps,
    policy: &DistributedToolPolicy,
) -> Result<ToolPolicy, DistributedCapabilityError> {
    let approval = match policy.approval.as_str() {
        "default" => ApprovalPolicy::Default,
        "never" => ApprovalPolicy::Never,
        "always" => ApprovalPolicy::Always,
        "on_request" => ApprovalPolicy::OnRequest,
        _ => {
            return Err(DistributedCapabilityError::new(
                "tool_policy.approval is unsupported",
            ))
        }
    };
    let can_use_tool = optional(
        &maps.tool_predicates,
        "tool_predicate",
        &policy.predicate_ref,
    )?;
    ToolPolicy {
        allowed_tools: policy.allowed_tools.clone(),
        disallowed_tools: policy.disallowed_tools.clone(),
        approval,
        can_use_tool,
        denied_side_effects: policy.denied_side_effects.clone(),
        denied_capability_tags: policy.denied_capability_tags.clone(),
        deny_terminal_tools: policy.deny_terminal_tools,
        denied_cost_dimensions: policy.denied_cost_dimensions.clone(),
    }
    .normalized()
    .map_err(|error| DistributedCapabilityError::new(error.to_string()))
}

fn resolve_checkpoint_extension(
    maps: &CapabilityMaps,
    descriptor: &DistributedCheckpointExtensionRef,
) -> Result<ResolvedDistributedCheckpointExtension, DistributedCapabilityError> {
    let extension = maps
        .checkpoint_extensions
        .get(&key(&descriptor.reference))
        .cloned()
        .ok_or_else(|| unknown("checkpoint_extension", &descriptor.reference))?;
    if extension.namespace() != descriptor.namespace {
        return Err(DistributedCapabilityError::new(format!(
            "checkpoint extension {}@{} namespace mismatch: expected {}, got {}",
            descriptor.reference.id,
            descriptor.reference.version,
            descriptor.namespace,
            extension.namespace()
        )));
    }
    if descriptor.required && !extension.required() {
        return Err(DistributedCapabilityError::new(format!(
            "required checkpoint extension {} is registered as optional",
            descriptor.namespace
        )));
    }
    Ok(ResolvedDistributedCheckpointExtension {
        descriptor: descriptor.clone(),
        extension,
    })
}

fn optional<T: Clone>(
    values: &BTreeMap<CapabilityKey, T>,
    kind: &str,
    reference: &Option<CapabilityRef>,
) -> Result<Option<T>, DistributedCapabilityError> {
    reference
        .as_ref()
        .map(|reference| {
            values
                .get(&key(reference))
                .cloned()
                .ok_or_else(|| unknown(kind, reference))
        })
        .transpose()
}

fn required_many<T: Clone>(
    values: &BTreeMap<CapabilityKey, T>,
    kind: &str,
    references: &[CapabilityRef],
) -> Result<Vec<T>, DistributedCapabilityError> {
    references
        .iter()
        .map(|reference| {
            values
                .get(&key(reference))
                .cloned()
                .ok_or_else(|| unknown(kind, reference))
        })
        .collect()
}

fn key(reference: &CapabilityRef) -> CapabilityKey {
    (reference.id.clone(), reference.version.clone())
}

fn unknown(kind: &str, reference: &CapabilityRef) -> DistributedCapabilityError {
    DistributedCapabilityError::new(format!(
        "unknown distributed capability {kind} {}@{}",
        reference.id, reference.version
    ))
}
