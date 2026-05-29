use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

use crate::llm::LlmClient;
use crate::runtime::sub_task_manager::SubTaskManager;
use crate::runtime::{RuntimeEventHandler, RuntimeLogHandler, StreamCallback};
use crate::tools::ToolRegistry;
use crate::types::{AgentTask, Metadata, SubAgentConfig, SubTaskRequest};
use crate::workspace::WorkspaceBackend;

#[derive(Clone, Default)]
pub(in crate::runtime) struct SubTaskCallbacks {
    pub(in crate::runtime) stream_callback: Option<StreamCallback>,
    pub(in crate::runtime) parent_log_handler: Option<RuntimeLogHandler>,
    pub(in crate::runtime) parent_event_handler: Option<RuntimeEventHandler>,
}

#[derive(Clone)]
pub(super) struct SubTaskRunContext {
    pub(super) llm_client: Arc<dyn LlmClient>,
    pub(super) tool_registry: ToolRegistry,
    pub(super) workspace_backend: Arc<dyn WorkspaceBackend>,
    pub(super) workspace_path: PathBuf,
    pub(super) parent_task: AgentTask,
    pub(super) parent_shared_state: BTreeMap<String, Value>,
    pub(super) sub_task_manager: SubTaskManager,
    pub(super) settings_file: Option<PathBuf>,
    pub(super) default_backend: Option<String>,
    pub(super) sub_agent_timeout_seconds: f64,
    pub(super) stream_callback: Option<StreamCallback>,
    pub(super) parent_log_handler: Option<RuntimeLogHandler>,
    pub(super) parent_event_handler: Option<RuntimeEventHandler>,
}

pub(super) struct SubTaskBuildInputs<'a> {
    pub(super) sub_task_id: &'a str,
    pub(super) sub_session_id: &'a str,
    pub(super) sub_agent_name: &'a str,
    pub(super) sub_agent: &'a SubAgentConfig,
    pub(super) resolved_model_id: &'a str,
    pub(super) request: &'a SubTaskRequest,
}

pub(super) struct ResolvedSubAgentClient {
    pub(super) llm_client: Arc<dyn LlmClient>,
    pub(super) model_id: String,
    pub(super) payload: BTreeMap<String, String>,
}

pub(super) struct RuntimeSubAgentSessionParts {
    pub(super) llm_client: Arc<dyn LlmClient>,
    pub(super) tool_registry: ToolRegistry,
    pub(super) workspace_path: PathBuf,
    pub(super) workspace_backend: Arc<dyn WorkspaceBackend>,
    pub(super) task_template: AgentTask,
    pub(super) agent_name: String,
    pub(super) session_id: String,
    pub(super) resolved: BTreeMap<String, String>,
    pub(super) stream_callback: Option<StreamCallback>,
    pub(super) parent_log_handler: Option<RuntimeLogHandler>,
    pub(super) parent_event_handler: Option<RuntimeEventHandler>,
}

#[derive(Default)]
pub(super) struct RuntimeSubAgentSessionState {
    pub(super) messages: Vec<crate::types::Message>,
    pub(super) shared_state: Metadata,
}
