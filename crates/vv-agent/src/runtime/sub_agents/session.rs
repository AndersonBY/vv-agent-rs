use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use crate::llm::LlmClient;
use crate::runtime::sub_agent_sessions::{
    SubAgentSession, SubAgentSessionListener, SubAgentSessionUnsubscribe,
};
use crate::runtime::{RuntimeEventHandler, RuntimeLogHandler, StreamCallback};
use crate::tools::ToolRegistry;
use crate::types::{AgentTask, SubTaskOutcome};
use crate::workspace::WorkspaceBackend;

use super::types::{RuntimeSubAgentSessionParts, RuntimeSubAgentSessionState};

mod events;
mod execution;
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
    stream_callback: Option<StreamCallback>,
    parent_log_handler: Option<RuntimeLogHandler>,
    parent_event_handler: Option<RuntimeEventHandler>,
    state: Mutex<RuntimeSubAgentSessionState>,
    running: Mutex<bool>,
    steering_queue: Arc<Mutex<VecDeque<String>>>,
    listeners: Arc<Mutex<BTreeMap<u64, SubAgentSessionListener>>>,
    next_listener_id: AtomicU64,
}

impl RuntimeSubAgentSession {
    pub(in crate::runtime::sub_agents) fn new(parts: RuntimeSubAgentSessionParts) -> Self {
        let task_id = parts.task_template.task_id.clone();
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
            stream_callback: parts.stream_callback,
            parent_log_handler: parts.parent_log_handler,
            parent_event_handler: parts.parent_event_handler,
            state: Mutex::new(RuntimeSubAgentSessionState::default()),
            running: Mutex::new(false),
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

    fn sanitize_for_resume(&self) -> usize {
        self.sanitize_state_for_resume()
    }

    fn continue_run(&self, prompt: &str) -> Result<SubTaskOutcome, String> {
        self.run_prompt(prompt)
    }

    fn subscribe(&self, listener: SubAgentSessionListener) -> Option<SubAgentSessionUnsubscribe> {
        Some(self.subscribe_listener(listener))
    }
}
