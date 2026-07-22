use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;

use crate::runtime::sub_agent_sessions::SubAgentSession;
use crate::runtime::{CancellationToken, ExecutionContext, RunEventHandler};
use crate::tools::ToolPolicy;
use crate::types::SubTaskOutcome;
use crate::workspace::WorkspaceBackend;
use crate::RunContext;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SubTaskLineage {
    pub parent_run_id: Option<String>,
    pub parent_tool_call_id: Option<String>,
}

#[derive(Clone, Default)]
pub struct SubTaskTurnSnapshot {
    pub cancellation_token: Option<CancellationToken>,
    pub event_handler: Option<RunEventHandler>,
    pub trace_id: Option<String>,
    pub parent_run_id: Option<String>,
    pub parent_tool_call_id: Option<String>,
    pub parent_execution_context: Option<ExecutionContext>,
    pub parent_run_context: Option<RunContext>,
    pub tool_policy: ToolPolicy,
}

thread_local! {
    static TURN_EVENT_HANDLER_STACK: RefCell<Vec<Option<RunEventHandler>>> =
        const { RefCell::new(Vec::new()) };
}

impl SubTaskTurnSnapshot {
    pub(crate) fn enter_event_handler_scope(event_handler: Option<RunEventHandler>) -> impl Drop {
        TURN_EVENT_HANDLER_STACK.with(|handlers| {
            handlers.borrow_mut().push(event_handler);
        });
        SubTaskTurnEventHandlerScope
    }

    pub(crate) fn current_event_handler() -> Option<Option<RunEventHandler>> {
        TURN_EVENT_HANDLER_STACK.with(|handlers| handlers.borrow().last().cloned())
    }
}

struct SubTaskTurnEventHandlerScope;

impl Drop for SubTaskTurnEventHandlerScope {
    fn drop(&mut self) {
        TURN_EVENT_HANDLER_STACK.with(|handlers| {
            handlers.borrow_mut().pop();
        });
    }
}

#[derive(Clone, Default)]
pub struct SubTaskSubmissionContext {
    pub workspace_backend: Option<Arc<dyn WorkspaceBackend>>,
    pub lineage: SubTaskLineage,
}

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
    pub parent_run_id: Option<String>,
    pub parent_tool_call_id: Option<String>,
    pub updated_at: String,
}
