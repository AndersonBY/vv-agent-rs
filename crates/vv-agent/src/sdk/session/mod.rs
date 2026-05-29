use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::runtime::background_sessions::BackgroundSessionSubscription;
use crate::runtime::sub_task_manager::SubTaskManager;
use crate::runtime::CancellationToken;
use crate::types::Metadata;

use super::types::{AgentDefinition, AgentRun};

mod events;
mod handles;
mod run;
mod state;
mod util;
mod watchers;

pub use handles::{SessionCancellationHandle, SessionSteeringHandle};
pub use state::{
    AgentSessionRunRequest, AgentSessionState, SessionEventHandler, SessionListenerId,
};

use util::{absolutize_workspace, generate_session_id, normalize_session_id};

pub struct AgentSession {
    execute_run: Arc<dyn Fn(AgentSessionRunRequest) -> Result<AgentRun, String> + Send + Sync>,
    session_id: String,
    _agent_name: String,
    _definition: AgentDefinition,
    workspace: PathBuf,
    shared_state: Metadata,
    messages: Vec<crate::types::Message>,
    latest_run: Option<AgentRun>,
    running: bool,
    listeners: Arc<Mutex<BTreeMap<SessionListenerId, SessionEventHandler>>>,
    background_command_subscriptions: Arc<Mutex<BTreeMap<String, BackgroundSessionSubscription>>>,
    next_listener_id: SessionListenerId,
    steering_queue: Arc<Mutex<VecDeque<String>>>,
    follow_up_queue: Arc<Mutex<VecDeque<String>>>,
    active_cancellation_token: Arc<Mutex<Option<CancellationToken>>>,
    sub_task_manager: SubTaskManager,
}

impl AgentSession {
    pub fn new(
        execute_run: Arc<dyn Fn(String) -> Result<AgentRun, String> + Send + Sync>,
        agent_name: impl Into<String>,
        definition: AgentDefinition,
        workspace: impl Into<PathBuf>,
    ) -> Self {
        let execute_run =
            Arc::new(move |request: AgentSessionRunRequest| execute_run(request.prompt));
        Self::new_with_context(execute_run, agent_name, definition, workspace)
    }

    pub fn new_with_session_id(
        execute_run: Arc<dyn Fn(String) -> Result<AgentRun, String> + Send + Sync>,
        session_id: impl Into<String>,
        agent_name: impl Into<String>,
        definition: AgentDefinition,
        workspace: impl Into<PathBuf>,
    ) -> Self {
        let execute_run =
            Arc::new(move |request: AgentSessionRunRequest| execute_run(request.prompt));
        Self::new_with_context_and_session_id(
            execute_run,
            session_id,
            agent_name,
            definition,
            workspace,
        )
    }

    pub fn new_with_context(
        execute_run: Arc<dyn Fn(AgentSessionRunRequest) -> Result<AgentRun, String> + Send + Sync>,
        agent_name: impl Into<String>,
        definition: AgentDefinition,
        workspace: impl Into<PathBuf>,
    ) -> Self {
        Self::new_with_context_and_session_id(
            execute_run,
            generate_session_id(),
            agent_name,
            definition,
            workspace,
        )
    }

    pub fn new_with_context_and_shared_state(
        execute_run: Arc<dyn Fn(AgentSessionRunRequest) -> Result<AgentRun, String> + Send + Sync>,
        agent_name: impl Into<String>,
        definition: AgentDefinition,
        workspace: impl Into<PathBuf>,
        shared_state: Metadata,
    ) -> Self {
        Self::new_with_context_and_session_id_and_shared_state(
            execute_run,
            generate_session_id(),
            agent_name,
            definition,
            workspace,
            shared_state,
        )
    }

    pub fn new_with_context_and_session_id(
        execute_run: Arc<dyn Fn(AgentSessionRunRequest) -> Result<AgentRun, String> + Send + Sync>,
        session_id: impl Into<String>,
        agent_name: impl Into<String>,
        definition: AgentDefinition,
        workspace: impl Into<PathBuf>,
    ) -> Self {
        Self::new_with_context_and_session_id_and_shared_state(
            execute_run,
            session_id,
            agent_name,
            definition,
            workspace,
            Metadata::new(),
        )
    }

    pub fn new_with_context_and_session_id_and_shared_state(
        execute_run: Arc<dyn Fn(AgentSessionRunRequest) -> Result<AgentRun, String> + Send + Sync>,
        session_id: impl Into<String>,
        agent_name: impl Into<String>,
        definition: AgentDefinition,
        workspace: impl Into<PathBuf>,
        shared_state: Metadata,
    ) -> Self {
        let mut shared_state = shared_state;
        shared_state
            .entry("todo_list".to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        let workspace = absolutize_workspace(workspace.into());
        Self {
            execute_run,
            session_id: normalize_session_id(session_id),
            _agent_name: agent_name.into(),
            _definition: definition,
            workspace,
            shared_state,
            messages: Vec::new(),
            latest_run: None,
            running: false,
            listeners: Arc::new(Mutex::new(BTreeMap::new())),
            background_command_subscriptions: Arc::new(Mutex::new(BTreeMap::new())),
            next_listener_id: 1,
            steering_queue: Arc::new(Mutex::new(VecDeque::new())),
            follow_up_queue: Arc::new(Mutex::new(VecDeque::new())),
            active_cancellation_token: Arc::new(Mutex::new(None)),
            sub_task_manager: SubTaskManager::default(),
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn agent_name(&self) -> &str {
        &self._agent_name
    }

    pub fn definition(&self) -> &AgentDefinition {
        &self._definition
    }

    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub fn messages(&self) -> Vec<crate::types::Message> {
        self.messages.clone()
    }

    pub fn shared_state(&self) -> Metadata {
        self.shared_state.clone()
    }

    pub fn latest_run(&self) -> Option<AgentRun> {
        self.latest_run.clone()
    }

    pub fn running(&self) -> bool {
        self.running
    }
}
