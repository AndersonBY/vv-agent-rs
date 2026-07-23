use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::llm::{LlmClient, LlmError};
use crate::runtime::backends::RuntimeExecutionBackend;
use crate::runtime::hooks::RuntimeHook;
use crate::runtime::lifecycle::AfterCycleHook;
use crate::tools::{build_default_registry, ToolRegistry};
use crate::types::{AgentResult, AgentTask};
use crate::workspace::{LocalWorkspaceBackend, WorkspaceBackend};

use super::{AgentRuntime, RuntimeRunControls};

impl<C: LlmClient> AgentRuntime<C> {
    pub fn new(llm_client: C) -> Self {
        Self {
            llm_client,
            tool_registry: build_default_registry(),
            default_workspace: None,
            event_handler: None,
            log_preview_chars: None,
            workspace_backend: Arc::new(LocalWorkspaceBackend::new(PathBuf::from("./workspace"))),
            hooks: Vec::new(),
            after_cycle_hooks: Vec::new(),
            execution_backend: RuntimeExecutionBackend::default(),
            settings_file: None,
            default_backend: None,
            sub_agent_timeout_seconds: 90.0,
            tool_policy: None,
            pending_tool_approval: None,
        }
    }

    pub fn with_tool_registry(mut self, tool_registry: ToolRegistry) -> Self {
        self.tool_registry = tool_registry;
        self
    }

    pub fn with_default_workspace(mut self, workspace: impl Into<PathBuf>) -> Self {
        self.default_workspace = Some(workspace.into());
        self
    }

    pub fn with_workspace_backend(mut self, backend: Arc<dyn WorkspaceBackend>) -> Self {
        self.workspace_backend = backend;
        self
    }

    pub fn with_hooks(mut self, hooks: Vec<Arc<dyn RuntimeHook>>) -> Self {
        self.hooks = hooks;
        self
    }

    pub fn with_after_cycle_hooks(mut self, hooks: Vec<Arc<dyn AfterCycleHook>>) -> Self {
        self.after_cycle_hooks = hooks;
        self
    }

    pub fn with_execution_backend(
        mut self,
        execution_backend: impl Into<RuntimeExecutionBackend>,
    ) -> Self {
        self.execution_backend = execution_backend.into();
        self
    }

    pub fn with_settings_file(mut self, settings_file: impl Into<PathBuf>) -> Self {
        self.settings_file = Some(settings_file.into());
        self
    }

    pub fn with_default_backend(mut self, default_backend: impl Into<String>) -> Self {
        self.default_backend = Some(default_backend.into());
        self
    }

    pub fn with_log_preview_chars(mut self, log_preview_chars: usize) -> Self {
        self.log_preview_chars = Some(log_preview_chars);
        self
    }

    pub fn with_sub_agent_timeout_seconds(mut self, timeout_seconds: f64) -> Self {
        self.sub_agent_timeout_seconds = timeout_seconds.max(1.0);
        self
    }

    pub(crate) fn with_tool_policy(mut self, tool_policy: crate::tools::ToolPolicy) -> Self {
        self.tool_policy = Some(tool_policy);
        self
    }

    pub(crate) fn with_pending_tool_approval(
        mut self,
        pending: Arc<Mutex<Option<crate::result::PendingToolApproval>>>,
    ) -> Self {
        self.pending_tool_approval = Some(pending);
        self
    }
}

impl<C: LlmClient + Clone + 'static> AgentRuntime<C> {
    pub fn set_tool_policy(&mut self, tool_policy: crate::tools::ToolPolicy) {
        self.tool_policy = Some(tool_policy);
    }

    pub fn run(&self, task: AgentTask) -> Result<AgentResult, LlmError> {
        self.run_with_controls(task, RuntimeRunControls::default())
    }
}
