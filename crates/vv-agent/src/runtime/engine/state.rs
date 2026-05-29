use std::path::PathBuf;
use std::sync::Arc;

use crate::llm::LlmClient;
use crate::runtime::backends::RuntimeExecutionBackend;
use crate::runtime::hooks::RuntimeHook;
use crate::tools::ToolRegistry;
use crate::workspace::WorkspaceBackend;

use super::RuntimeLogHandler;

pub struct AgentRuntime<C: LlmClient> {
    pub llm_client: C,
    pub tool_registry: ToolRegistry,
    pub default_workspace: Option<PathBuf>,
    pub log_handler: Option<RuntimeLogHandler>,
    pub log_preview_chars: Option<usize>,
    pub workspace_backend: Arc<dyn WorkspaceBackend>,
    pub hooks: Vec<Arc<dyn RuntimeHook>>,
    pub execution_backend: RuntimeExecutionBackend,
    pub settings_file: Option<PathBuf>,
    pub default_backend: Option<String>,
    pub sub_agent_timeout_seconds: f64,
}
