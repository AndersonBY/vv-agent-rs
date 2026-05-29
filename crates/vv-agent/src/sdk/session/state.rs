use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::runtime::sub_task_manager::SubTaskManager;
use crate::runtime::{
    BeforeCycleMessageProvider, CancellationToken, InterruptionMessageProvider, StreamCallback,
};
use crate::types::Metadata;

use super::AgentSession;
use crate::sdk::types::AgentRun;

#[derive(Debug, Clone)]
pub struct AgentSessionState {
    pub running: bool,
    pub session_id: String,
    pub workspace: PathBuf,
    pub messages: Vec<crate::types::Message>,
    pub shared_state: Metadata,
    pub latest_run: Option<AgentRun>,
}

pub type SessionEventHandler = Arc<dyn Fn(&str, &BTreeMap<String, Value>) + Send + Sync + 'static>;
pub type SessionListenerId = u64;

#[derive(Clone, Default)]
pub struct AgentSessionRunRequest {
    pub prompt: String,
    pub task_name: Option<String>,
    pub workspace: Option<PathBuf>,
    pub initial_messages: Vec<crate::types::Message>,
    pub shared_state: Metadata,
    pub metadata: Metadata,
    pub runtime_event_handler: Option<SessionEventHandler>,
    pub before_cycle_messages: Option<BeforeCycleMessageProvider>,
    pub interruption_messages: Option<InterruptionMessageProvider>,
    pub steering_queue: Option<Arc<Mutex<VecDeque<String>>>>,
    pub cancellation_token: Option<CancellationToken>,
    pub stream_callback: Option<StreamCallback>,
    pub sub_task_manager: Option<SubTaskManager>,
}

impl AgentSessionRunRequest {
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            task_name: None,
            workspace: None,
            initial_messages: Vec::new(),
            shared_state: Metadata::new(),
            metadata: Metadata::new(),
            runtime_event_handler: None,
            before_cycle_messages: None,
            interruption_messages: None,
            steering_queue: None,
            cancellation_token: None,
            stream_callback: None,
            sub_task_manager: None,
        }
    }
}

impl AgentSession {
    pub fn state(&self) -> AgentSessionState {
        AgentSessionState {
            running: self.running,
            session_id: self.session_id.clone(),
            workspace: self.workspace.clone(),
            messages: self.messages.clone(),
            shared_state: self.shared_state.clone(),
            latest_run: self.latest_run.clone(),
        }
    }
}
