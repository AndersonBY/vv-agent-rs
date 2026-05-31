use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

use crate::execution_mode::ExecutionMode;
use crate::model::{ModelProvider, ModelRef};
use crate::model_settings::ModelSettings;
use crate::runtime::backends::RuntimeExecutionBackend;
use crate::runtime::{CancellationToken, RuntimeHook};
use crate::sessions::Session;
use crate::tools::ToolPolicy;
use crate::tracing::TraceSink;
use crate::types::Metadata;
use crate::workspace::WorkspaceBackend;

#[derive(Clone)]
pub struct RunConfig {
    pub model: Option<ModelRef>,
    pub model_provider: Option<Arc<dyn ModelProvider>>,
    pub model_settings: Option<ModelSettings>,
    pub workspace: Option<PathBuf>,
    pub workspace_backend: Option<Arc<dyn WorkspaceBackend>>,
    pub session: Option<Arc<dyn Session>>,
    pub max_cycles: Option<u32>,
    pub tool_policy: ToolPolicy,
    pub execution_backend: Option<RuntimeExecutionBackend>,
    pub cancellation_token: Option<CancellationToken>,
    pub hooks: Vec<Arc<dyn RuntimeHook>>,
    pub trace_sink: Option<Arc<dyn TraceSink>>,
    pub app_state: Option<Arc<dyn std::any::Any + Send + Sync>>,
    pub metadata: Metadata,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            model: None,
            model_provider: None,
            model_settings: None,
            workspace: None,
            workspace_backend: None,
            session: None,
            max_cycles: None,
            tool_policy: ToolPolicy::default(),
            execution_backend: None,
            cancellation_token: None,
            hooks: Vec::new(),
            trace_sink: None,
            app_state: None,
            metadata: BTreeMap::new(),
        }
    }
}

impl RunConfig {
    pub fn builder() -> RunConfigBuilder {
        RunConfigBuilder::default()
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

    pub fn max_cycles(mut self, max_cycles: u32) -> Self {
        self.config.max_cycles = Some(max_cycles);
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

    pub fn trace_sink(mut self, sink: Arc<dyn TraceSink>) -> Self {
        self.config.trace_sink = Some(sink);
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
