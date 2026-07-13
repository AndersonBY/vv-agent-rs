use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use sha2::{Digest, Sha256};

use crate::approval::{ApprovalBroker, ApprovalProvider};
use crate::llm::LlmClient;
use crate::memory::MemoryProvider;
use crate::runtime::engine::RuntimeEventHandler;
use crate::runtime::hooks::RuntimeHook;
use crate::runtime::sub_task_manager::SubTaskManager;
use crate::runtime::CancellationToken;
use crate::tools::{ApprovalPolicy, CanUseToolPredicate, ToolPolicy, ToolRegistry};
use crate::workspace::WorkspaceBackend;

use super::{CapabilityRef, DistributedCapabilities, DistributedToolPolicy, ToolsetRef};

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
    event_sinks: BTreeMap<CapabilityKey, RuntimeEventHandler>,
    app_states: BTreeMap<CapabilityKey, Arc<dyn std::any::Any + Send + Sync>>,
    memory_providers: BTreeMap<CapabilityKey, Arc<dyn MemoryProvider>>,
    hooks: BTreeMap<CapabilityKey, Arc<dyn RuntimeHook>>,
    observers: BTreeMap<CapabilityKey, RuntimeEventHandler>,
    sub_task_managers: BTreeMap<CapabilityKey, SubTaskManager>,
    tool_predicates: BTreeMap<CapabilityKey, CanUseToolPredicate>,
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

    pub fn register_event_sink(&self, reference: CapabilityRef, sink: RuntimeEventHandler) {
        self.write_unpoisoned()
            .event_sinks
            .insert(key(&reference), sink);
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

    pub fn register_observer(&self, reference: CapabilityRef, observer: RuntimeEventHandler) {
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
        let observers = required_many(&maps.observers, "observer", &capabilities.observer_refs)?;
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
            app_state,
            memory_providers,
            hooks,
            observers,
            sub_task_manager,
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
    pub event_sink: Option<RuntimeEventHandler>,
    pub app_state: Option<Arc<dyn std::any::Any + Send + Sync>>,
    pub memory_providers: Vec<Arc<dyn MemoryProvider>>,
    pub hooks: Vec<Arc<dyn RuntimeHook>>,
    pub observers: Vec<RuntimeEventHandler>,
    pub sub_task_manager: Option<SubTaskManager>,
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
    Ok(ToolPolicy {
        allowed_tools: policy.allowed_tools.clone(),
        disallowed_tools: policy.disallowed_tools.clone(),
        approval,
        can_use_tool,
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
