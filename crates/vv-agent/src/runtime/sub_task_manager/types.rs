use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;

use crate::runtime::sub_agent_sessions::SubAgentSession;
use crate::types::SubTaskOutcome;
use crate::workspace::WorkspaceBackend;

#[derive(Clone)]
pub struct SubTaskSessionAttachment {
    pub task_id: String,
    pub session_id: String,
    pub agent_name: String,
    pub task_title: String,
    pub workspace_backend: Arc<dyn WorkspaceBackend>,
    pub session: Arc<dyn SubAgentSession>,
    pub resolved: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct ManagedSubTaskSnapshot {
    pub task_id: String,
    pub session_id: String,
    pub agent_name: String,
    pub task_title: String,
    pub status: String,
    pub running: bool,
    pub outcome: Option<SubTaskOutcome>,
    pub resolved: BTreeMap<String, String>,
    pub current_cycle_index: Option<u32>,
    pub recent_activity: Option<String>,
    pub latest_cycle: Option<Value>,
    pub latest_tool_call: Option<Value>,
    pub updated_at: String,
}
