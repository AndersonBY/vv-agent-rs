use std::path::PathBuf;
use std::sync::Arc;

use crate::llm::LlmClient;
use crate::runtime::backends::RuntimeExecutionBackend;
use crate::tools::{build_default_registry, ToolRegistry};
use crate::workspace::LocalWorkspaceBackend;

use super::AgentRuntime;

impl<C: LlmClient> AgentRuntime<C> {
    pub fn new(llm_client: C) -> Self {
        Self {
            llm_client,
            tool_registry: build_default_registry(),
            default_workspace: None,
            log_handler: None,
            log_preview_chars: None,
            workspace_backend: Arc::new(LocalWorkspaceBackend::new(PathBuf::from("./workspace"))),
            hooks: Vec::new(),
            execution_backend: RuntimeExecutionBackend::default(),
            settings_file: None,
            default_backend: None,
            sub_agent_timeout_seconds: 90.0,
        }
    }

    pub fn with_tool_registry(mut self, tool_registry: ToolRegistry) -> Self {
        self.tool_registry = tool_registry;
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
}
