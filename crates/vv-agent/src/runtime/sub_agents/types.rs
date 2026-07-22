use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

use crate::budget::RunBudgetLimits;
use crate::llm::LlmClient;
use crate::model::ModelProvider;
use crate::model::ModelRef;
use crate::runtime::sub_task_manager::SubTaskManager;
use crate::runtime::{CancellationToken, ExecutionContext, RunEventHandler};
use crate::tools::ToolPolicy;
use crate::tools::ToolRegistry;
use crate::types::{AgentTask, Metadata, SubAgentConfig, SubTaskRequest};
use crate::workspace::WorkspaceBackend;
use crate::RunContext;

#[derive(Clone, Default)]
pub(in crate::runtime) struct SubTaskRunControls {
    pub(in crate::runtime) parent_cancellation_token: Option<CancellationToken>,
    pub(in crate::runtime) event_handler: Option<RunEventHandler>,
    pub(in crate::runtime) parent_execution_context: Option<ExecutionContext>,
    pub(in crate::runtime) model_provider: Option<Arc<dyn ModelProvider>>,
    pub(in crate::runtime) parent_run_context: Option<RunContext>,
    pub(in crate::runtime) tool_policy: Option<ToolPolicy>,
    pub(in crate::runtime) budget_limits: Option<RunBudgetLimits>,
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
    pub(super) parent_cancellation_token: Option<CancellationToken>,
    pub(super) settings_file: Option<PathBuf>,
    pub(super) default_backend: Option<String>,
    pub(super) sub_agent_timeout_seconds: f64,
    pub(super) event_handler: Option<RunEventHandler>,
    pub(super) parent_execution_context: Option<ExecutionContext>,
    pub(super) model_provider: Option<Arc<dyn ModelProvider>>,
    pub(super) parent_run_context: Option<RunContext>,
    pub(super) tool_policy: Option<ToolPolicy>,
    pub(super) budget_limits: Option<RunBudgetLimits>,
}

pub(super) struct SubTaskBuildInputs<'a> {
    pub(super) lifecycle: &'a SubRunLifecycle,
    pub(super) sub_agent: &'a SubAgentConfig,
    pub(super) resolved_model_id: &'a str,
    pub(super) resolved_native_multimodal: bool,
    pub(super) resolved_context_length: Option<u64>,
    pub(super) resolved_max_output_tokens: Option<u64>,
    pub(super) request: &'a SubTaskRequest,
}

pub(super) struct ResolvedSubAgentClient {
    pub(super) llm_client: Arc<dyn LlmClient>,
    pub(super) model_id: String,
    pub(super) run_model_ref: ModelRef,
    pub(super) native_multimodal: bool,
    pub(super) context_length: Option<u64>,
    pub(super) max_output_tokens: Option<u64>,
    pub(super) payload: BTreeMap<String, String>,
}

#[derive(Clone)]
pub(super) struct SubRunLifecycle {
    pub(super) run_id: String,
    pub(super) trace_id: String,
    pub(super) parent_run_id: String,
    pub(super) parent_tool_call_id: String,
    pub(super) task_id: String,
    pub(super) session_id: String,
    pub(super) agent_name: String,
    pub(super) parent_task_id: String,
    pub(super) model: String,
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
    pub(super) settings_file: Option<PathBuf>,
    pub(super) default_backend: Option<String>,
    pub(super) parent_cancellation_token: Option<CancellationToken>,
    pub(super) event_handler: Option<RunEventHandler>,
    pub(super) parent_execution_context: Option<ExecutionContext>,
    pub(super) model_provider: Option<Arc<dyn ModelProvider>>,
    pub(super) run_model_ref: ModelRef,
    pub(super) tool_policy: ToolPolicy,
    pub(super) budget_limits: Option<RunBudgetLimits>,
    pub(super) initial_lifecycle: SubRunLifecycle,
}

#[derive(Default)]
pub(super) struct RuntimeSubAgentSessionState {
    pub(super) messages: Vec<crate::types::Message>,
    pub(super) shared_state: Metadata,
}
