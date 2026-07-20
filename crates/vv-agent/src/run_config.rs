use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::approval::{ApprovalBroker, ApprovalProvider};
use crate::budget::{HostCostMeter, RunBudgetLimits};
use crate::checkpoint::{CheckpointConfig, CheckpointExtension, ReconciliationProvider};
use crate::context_providers::ContextProvider;
use crate::event_store::RunEventStore;
use crate::execution_mode::ExecutionMode;
use crate::llm::LlmStreamCallback;
use crate::memory::MemoryProvider;
use crate::model::{ModelProvider, ModelRef};
use crate::model_settings::ModelSettings;
use crate::runtime::backends::RuntimeExecutionBackend;
use crate::runtime::{
    AfterCycleHook, BeforeCycleMessageProvider, CancellationToken, InterruptionMessageProvider,
    RuntimeEventHandler, RuntimeHook, SubTaskManager,
};
use crate::sessions::Session;
use crate::tools::{ToolPolicy, ToolRegistry};
use crate::tracing::TraceSink;
use crate::types::{Message, Metadata, NoToolPolicy};
use crate::workspace::WorkspaceBackend;

pub type ToolRegistryFactory = Arc<dyn Fn() -> ToolRegistry + Send + Sync + 'static>;

pub(crate) const INITIAL_BUDGET_USAGE_METADATA_KEY: &str = "_vv_agent_initial_budget_usage";

pub(crate) const MAX_CYCLES_RANGE_ERROR: &str = "max_cycles must be between 1 and 4294967295";

pub(crate) fn validate_max_cycles(max_cycles: u32) -> Result<u32, String> {
    if max_cycles == 0 {
        return Err(MAX_CYCLES_RANGE_ERROR.to_string());
    }
    Ok(max_cycles)
}

#[derive(Clone, Default)]
pub struct RunConfig {
    pub model: Option<ModelRef>,
    pub model_provider: Option<Arc<dyn ModelProvider>>,
    pub model_settings: Option<ModelSettings>,
    pub workspace: Option<PathBuf>,
    pub workspace_backend: Option<Arc<dyn WorkspaceBackend>>,
    pub session: Option<Arc<dyn Session>>,
    pub initial_messages: Option<Vec<Message>>,
    pub max_cycles: Option<u32>,
    pub max_handoffs: Option<u32>,
    pub no_tool_policy: Option<NoToolPolicy>,
    pub tool_policy: ToolPolicy,
    pub execution_backend: Option<RuntimeExecutionBackend>,
    pub cancellation_token: Option<CancellationToken>,
    pub hooks: Vec<Arc<dyn RuntimeHook>>,
    pub after_cycle_hooks: Vec<Arc<dyn AfterCycleHook>>,
    pub trace_sink: Option<Arc<dyn TraceSink>>,
    pub trace_id: Option<String>,
    pub workflow_name: Option<String>,
    pub event_store: Option<Arc<dyn RunEventStore>>,
    pub event_store_fail_closed: bool,
    pub approval_provider: Option<Arc<dyn ApprovalProvider>>,
    pub approval_timeout: Option<Duration>,
    pub approval_broker: Option<ApprovalBroker>,
    pub context_providers: Vec<Arc<dyn ContextProvider>>,
    pub max_context_chars: Option<usize>,
    pub memory_providers: Vec<Arc<dyn MemoryProvider>>,
    pub app_state: Option<Arc<dyn std::any::Any + Send + Sync>>,
    pub initial_shared_state: Metadata,
    pub tool_registry_factory: Option<ToolRegistryFactory>,
    pub log_preview_chars: Option<usize>,
    pub debug_dump_dir: Option<PathBuf>,
    pub before_cycle_messages: Option<BeforeCycleMessageProvider>,
    pub interruption_messages: Option<InterruptionMessageProvider>,
    pub sub_task_manager: Option<SubTaskManager>,
    pub runtime_log_handler: Option<RuntimeEventHandler>,
    pub runtime_stream_callback: Option<LlmStreamCallback>,
    pub budget_limits: Option<RunBudgetLimits>,
    pub host_cost_meter: Option<Arc<dyn HostCostMeter>>,
    pub checkpoint_config: Option<CheckpointConfig>,
    pub checkpoint_extensions: Vec<Arc<dyn CheckpointExtension>>,
    pub reconciliation_provider: Option<Arc<dyn ReconciliationProvider>>,
    pub metadata: Metadata,
}

impl RunConfig {
    pub fn builder() -> RunConfigBuilder {
        RunConfigBuilder::default()
    }

    pub(crate) fn for_background_child(&self, shared_state: Metadata) -> Self {
        let mut child = self.clone();
        child.model = None;
        child.model_settings = None;
        child.session = None;
        child.initial_messages = None;
        child.cancellation_token = self
            .cancellation_token
            .as_ref()
            .map(CancellationToken::child);
        child.initial_shared_state = shared_state;
        child.before_cycle_messages = None;
        child.interruption_messages = None;
        child.sub_task_manager = None;
        child.runtime_log_handler = None;
        child.runtime_stream_callback = None;
        child.host_cost_meter = None;
        child.checkpoint_config = None;
        child.checkpoint_extensions.clear();
        child.reconciliation_provider = None;
        child.metadata.remove(INITIAL_BUDGET_USAGE_METADATA_KEY);
        child
    }
}

#[derive(Default)]
pub struct RunConfigBuilder {
    config: RunConfig,
}

impl RunConfigBuilder {
    pub fn model(mut self, model: ModelRef) -> Self {
        self.config.model = Some(model);
        self
    }

    pub fn model_provider(mut self, provider: impl ModelProvider + 'static) -> Self {
        self.config.model_provider = Some(Arc::new(provider));
        self
    }

    pub fn model_provider_arc(mut self, provider: Arc<dyn ModelProvider>) -> Self {
        self.config.model_provider = Some(provider);
        self
    }

    pub fn model_settings(mut self, settings: ModelSettings) -> Self {
        self.config.model_settings = Some(settings);
        self
    }

    pub fn workspace(mut self, workspace: impl Into<PathBuf>) -> Self {
        self.config.workspace = Some(workspace.into());
        self
    }

    pub fn workspace_backend(mut self, backend: Arc<dyn WorkspaceBackend>) -> Self {
        self.config.workspace_backend = Some(backend);
        self
    }

    pub fn session(mut self, session: impl Session + 'static) -> Self {
        self.config.session = Some(Arc::new(session));
        self
    }

    pub fn session_arc(mut self, session: Arc<dyn Session>) -> Self {
        self.config.session = Some(session);
        self
    }

    pub fn initial_messages(mut self, messages: Vec<Message>) -> Self {
        self.config.initial_messages = Some(messages);
        self
    }

    pub fn max_cycles(mut self, max_cycles: u32) -> Self {
        self.config.max_cycles = Some(max_cycles);
        self
    }

    pub fn max_handoffs(mut self, max_handoffs: u32) -> Self {
        self.config.max_handoffs = Some(max_handoffs);
        self
    }

    pub fn no_tool_policy(mut self, policy: NoToolPolicy) -> Self {
        self.config.no_tool_policy = Some(policy);
        self
    }

    pub fn tool_policy(mut self, tool_policy: ToolPolicy) -> Self {
        self.config.tool_policy = tool_policy;
        self
    }

    pub fn execution_backend(mut self, execution_backend: RuntimeExecutionBackend) -> Self {
        self.config.execution_backend = Some(execution_backend);
        self
    }

    pub fn execution_mode(mut self, execution_mode: ExecutionMode) -> Self {
        self.config.execution_backend = Some(execution_mode.into());
        self
    }

    pub fn cancellation_token(mut self, cancellation_token: CancellationToken) -> Self {
        self.config.cancellation_token = Some(cancellation_token);
        self
    }

    pub fn hook(mut self, hook: Arc<dyn RuntimeHook>) -> Self {
        self.config.hooks.push(hook);
        self
    }

    pub fn after_cycle_hook(mut self, hook: impl AfterCycleHook + 'static) -> Self {
        self.config.after_cycle_hooks.push(Arc::new(hook));
        self
    }

    pub fn after_cycle_hook_arc(mut self, hook: Arc<dyn AfterCycleHook>) -> Self {
        self.config.after_cycle_hooks.push(hook);
        self
    }

    pub fn trace_sink(mut self, sink: Arc<dyn TraceSink>) -> Self {
        self.config.trace_sink = Some(sink);
        self
    }

    pub fn trace_id(mut self, trace_id: impl Into<String>) -> Self {
        self.config.trace_id = Some(trace_id.into());
        self
    }

    pub fn workflow_name(mut self, workflow_name: impl Into<String>) -> Self {
        self.config.workflow_name = Some(workflow_name.into());
        self
    }

    pub fn event_store(mut self, store: Arc<dyn RunEventStore>) -> Self {
        self.config.event_store = Some(store);
        self
    }

    pub fn event_store_fail_closed(mut self, fail_closed: bool) -> Self {
        self.config.event_store_fail_closed = fail_closed;
        self
    }

    pub fn approval_provider(mut self, provider: Arc<dyn ApprovalProvider>) -> Self {
        self.config.approval_provider = Some(provider);
        self
    }

    pub fn approval_timeout(mut self, timeout: Duration) -> Self {
        self.config.approval_timeout = Some(timeout);
        self
    }

    pub fn approval_broker(mut self, broker: ApprovalBroker) -> Self {
        self.config.approval_broker = Some(broker);
        self
    }

    pub fn context_provider(mut self, provider: Arc<dyn ContextProvider>) -> Self {
        self.config.context_providers.push(provider);
        self
    }

    pub fn max_context_chars(mut self, max_chars: usize) -> Self {
        self.config.max_context_chars = Some(max_chars);
        self
    }

    pub fn memory_provider(mut self, provider: Arc<dyn MemoryProvider>) -> Self {
        self.config.memory_providers.push(provider);
        self
    }

    pub fn app_state<T>(mut self, app_state: T) -> Self
    where
        T: Send + Sync + 'static,
    {
        self.config.app_state = Some(Arc::new(app_state));
        self
    }

    pub fn app_state_arc(mut self, app_state: Arc<dyn std::any::Any + Send + Sync>) -> Self {
        self.config.app_state = Some(app_state);
        self
    }

    pub fn initial_shared_state(mut self, shared_state: Metadata) -> Self {
        self.config.initial_shared_state = shared_state;
        self
    }

    pub fn shared_state(self, shared_state: Metadata) -> Self {
        self.initial_shared_state(shared_state)
    }

    pub fn tool_registry_factory(
        mut self,
        factory: impl Fn() -> ToolRegistry + Send + Sync + 'static,
    ) -> Self {
        self.config.tool_registry_factory = Some(Arc::new(factory));
        self
    }

    pub fn tool_registry_factory_arc(mut self, factory: ToolRegistryFactory) -> Self {
        self.config.tool_registry_factory = Some(factory);
        self
    }

    pub fn log_preview_chars(mut self, max_chars: usize) -> Self {
        self.config.log_preview_chars = Some(max_chars);
        self
    }

    pub fn debug_dump_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.config.debug_dump_dir = Some(path.into());
        self
    }

    pub fn before_cycle_messages(
        mut self,
        provider: impl Fn(u32, &[Message], &Metadata) -> Vec<Message> + Send + Sync + 'static,
    ) -> Self {
        self.config.before_cycle_messages = Some(Arc::new(provider));
        self
    }

    pub fn before_cycle_messages_arc(mut self, provider: BeforeCycleMessageProvider) -> Self {
        self.config.before_cycle_messages = Some(provider);
        self
    }

    pub fn interruption_messages(
        mut self,
        provider: impl Fn() -> Vec<Message> + Send + Sync + 'static,
    ) -> Self {
        self.config.interruption_messages = Some(Arc::new(provider));
        self
    }

    pub fn interruption_messages_arc(mut self, provider: InterruptionMessageProvider) -> Self {
        self.config.interruption_messages = Some(provider);
        self
    }

    pub fn sub_task_manager(mut self, manager: SubTaskManager) -> Self {
        self.config.sub_task_manager = Some(manager);
        self
    }

    pub fn runtime_log_handler(
        mut self,
        handler: impl Fn(&str, &Metadata) + Send + Sync + 'static,
    ) -> Self {
        self.config.runtime_log_handler = Some(Arc::new(handler));
        self
    }

    pub fn runtime_log_handler_arc(mut self, handler: RuntimeEventHandler) -> Self {
        self.config.runtime_log_handler = Some(handler);
        self
    }

    pub fn runtime_stream_callback(
        mut self,
        callback: impl Fn(&Metadata) + Send + Sync + 'static,
    ) -> Self {
        self.config.runtime_stream_callback = Some(Arc::new(callback));
        self
    }

    pub fn runtime_stream_callback_arc(mut self, callback: LlmStreamCallback) -> Self {
        self.config.runtime_stream_callback = Some(callback);
        self
    }

    pub fn budget_limits(mut self, limits: RunBudgetLimits) -> Self {
        self.config.budget_limits = Some(limits);
        self
    }

    pub fn host_cost_meter(mut self, meter: impl HostCostMeter + 'static) -> Self {
        self.config.host_cost_meter = Some(Arc::new(meter));
        self
    }

    pub fn host_cost_meter_arc(mut self, meter: Arc<dyn HostCostMeter>) -> Self {
        self.config.host_cost_meter = Some(meter);
        self
    }

    pub fn checkpoint_config(mut self, checkpoint_config: CheckpointConfig) -> Self {
        self.config.checkpoint_config = Some(checkpoint_config);
        self
    }

    pub fn checkpoint_extension(mut self, extension: impl CheckpointExtension + 'static) -> Self {
        self.config.checkpoint_extensions.push(Arc::new(extension));
        self
    }

    pub fn checkpoint_extension_arc(mut self, extension: Arc<dyn CheckpointExtension>) -> Self {
        self.config.checkpoint_extensions.push(extension);
        self
    }

    pub fn reconciliation_provider(
        mut self,
        provider: impl ReconciliationProvider + 'static,
    ) -> Self {
        self.config.reconciliation_provider = Some(Arc::new(provider));
        self
    }

    pub fn reconciliation_provider_arc(
        mut self,
        provider: Arc<dyn ReconciliationProvider>,
    ) -> Self {
        self.config.reconciliation_provider = Some(provider);
        self
    }

    pub fn metadata(mut self, key: impl Into<String>, value: Value) -> Self {
        self.config.metadata.insert(key.into(), value);
        self
    }

    pub fn build(self) -> RunConfig {
        self.config
    }
}
