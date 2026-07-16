use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex};

use crate::budget::RunBudgetLimits;
use crate::llm::LlmClient;
use crate::runtime::sub_agent_sessions::{
    SubAgentSession, SubAgentSessionListener, SubAgentSessionUnsubscribe,
};
use crate::runtime::sub_task_manager::SubTaskTurnSnapshot;
use crate::runtime::{CancellationToken, RuntimeEventHandler, RuntimeLogHandler, StreamCallback};
use crate::tools::ToolPolicy;
use crate::tools::ToolRegistry;
use crate::types::{AgentTask, SubTaskOutcome};
use crate::workspace::WorkspaceBackend;

use super::types::SubRunLifecycle;
use super::types::{RuntimeSubAgentSessionParts, RuntimeSubAgentSessionState};
use crate::model::ModelProvider;
use crate::model::ModelRef;
use crate::runtime::ExecutionContext;

mod events;
mod execution;
mod projection;
mod state;
mod subscription;

pub(in crate::runtime::sub_agents) struct RuntimeSubAgentSession {
    llm_client: Arc<dyn LlmClient>,
    tool_registry: ToolRegistry,
    workspace_path: PathBuf,
    workspace_backend: Arc<dyn WorkspaceBackend>,
    pub(in crate::runtime::sub_agents) task_template: AgentTask,
    task_id: String,
    agent_name: String,
    session_id: String,
    resolved: BTreeMap<String, String>,
    settings_file: Option<PathBuf>,
    default_backend: Option<String>,
    parent_cancellation_token: Option<CancellationToken>,
    stream_callback: Option<StreamCallback>,
    parent_log_handler: Option<RuntimeLogHandler>,
    parent_event_handler: Option<RuntimeEventHandler>,
    parent_execution_context: Option<ExecutionContext>,
    model_provider: Option<Arc<dyn ModelProvider>>,
    run_model_ref: ModelRef,
    tool_policy: ToolPolicy,
    budget_limits: Option<RunBudgetLimits>,
    lifecycle_template: SubRunLifecycle,
    initial_lifecycle_pending: AtomicBool,
    state: Mutex<RuntimeSubAgentSessionState>,
    running: Mutex<bool>,
    active_cancellation_token: Mutex<Option<CancellationToken>>,
    steering_queue: Arc<Mutex<VecDeque<String>>>,
    listeners: Arc<Mutex<BTreeMap<u64, SubAgentSessionListener>>>,
    next_listener_id: AtomicU64,
}

impl RuntimeSubAgentSession {
    pub(in crate::runtime::sub_agents) fn new(parts: RuntimeSubAgentSessionParts) -> Self {
        let task_id = parts.task_template.task_id.clone();
        let state = RuntimeSubAgentSessionState {
            messages: parts.task_template.initial_messages.clone(),
            shared_state: parts.task_template.initial_shared_state.clone(),
        };
        let lifecycle_template = parts.initial_lifecycle;
        Self {
            llm_client: parts.llm_client,
            tool_registry: parts.tool_registry,
            workspace_path: parts.workspace_path,
            workspace_backend: parts.workspace_backend,
            task_template: parts.task_template,
            task_id,
            agent_name: parts.agent_name,
            session_id: parts.session_id,
            resolved: parts.resolved,
            settings_file: parts.settings_file,
            default_backend: parts.default_backend,
            parent_cancellation_token: parts.parent_cancellation_token,
            stream_callback: parts.stream_callback,
            parent_log_handler: parts.parent_log_handler,
            parent_event_handler: parts.parent_event_handler,
            parent_execution_context: parts.parent_execution_context,
            model_provider: parts.model_provider,
            run_model_ref: parts.run_model_ref,
            tool_policy: parts.tool_policy,
            budget_limits: parts.budget_limits,
            lifecycle_template,
            initial_lifecycle_pending: AtomicBool::new(true),
            state: Mutex::new(state),
            running: Mutex::new(false),
            active_cancellation_token: Mutex::new(None),
            steering_queue: Arc::new(Mutex::new(VecDeque::new())),
            listeners: Arc::new(Mutex::new(BTreeMap::new())),
            next_listener_id: AtomicU64::new(1),
        }
    }
}

impl SubAgentSession for RuntimeSubAgentSession {
    fn steer(&self, prompt: &str) -> Result<(), String> {
        self.queue_steering(prompt)
    }

    fn cancel(&self) -> bool {
        self.cancel_active_run()
    }

    fn sanitize_for_resume(&self) -> usize {
        self.sanitize_state_for_resume()
    }

    fn continue_run(&self, prompt: &str) -> Result<SubTaskOutcome, String> {
        self.run_prompt(prompt, None)
    }

    fn continue_run_with_snapshot(
        &self,
        prompt: &str,
        snapshot: SubTaskTurnSnapshot,
    ) -> Result<SubTaskOutcome, String> {
        self.run_prompt(prompt, Some(snapshot))
    }

    fn subscribe(&self, listener: SubAgentSessionListener) -> Option<SubAgentSessionUnsubscribe> {
        Some(self.subscribe_listener(listener))
    }
}
