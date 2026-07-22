use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::llm::LlmClient;
use crate::runtime::backends::RuntimeExecutionBackend;
use crate::runtime::hooks::RuntimeHook;
use crate::runtime::lifecycle::AfterCycleHook;
use crate::tools::{ToolPolicy, ToolRegistry};
use crate::workspace::WorkspaceBackend;

use super::RunEventHandler;

pub struct AgentRuntime<C: LlmClient> {
    pub llm_client: C,
    pub tool_registry: ToolRegistry,
    pub default_workspace: Option<PathBuf>,
    pub event_handler: Option<RunEventHandler>,
    pub log_preview_chars: Option<usize>,
    pub workspace_backend: Arc<dyn WorkspaceBackend>,
    pub hooks: Vec<Arc<dyn RuntimeHook>>,
    pub after_cycle_hooks: Vec<Arc<dyn AfterCycleHook>>,
    pub execution_backend: RuntimeExecutionBackend,
    pub settings_file: Option<PathBuf>,
    pub default_backend: Option<String>,
    pub sub_agent_timeout_seconds: f64,
    pub(crate) tool_policy: Option<ToolPolicy>,
    pub(crate) pending_tool_approval:
        Option<Arc<Mutex<Option<crate::result::PendingToolApproval>>>>,
}
