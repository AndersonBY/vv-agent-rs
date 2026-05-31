use serde::{Deserialize, Serialize};

use crate::types::AgentStatus;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunEvent {
    RunStarted {
        run_id: String,
        agent_name: String,
    },
    AgentStarted {
        run_id: String,
        agent_name: String,
        cycle_index: u32,
    },
    LlmStarted {
        run_id: String,
        model: String,
        cycle_index: u32,
    },
    AssistantDelta {
        run_id: String,
        delta: String,
        cycle_index: u32,
    },
    ToolStarted {
        run_id: String,
        tool_call_id: String,
        tool_name: String,
        cycle_index: u32,
    },
    ToolFinished {
        run_id: String,
        tool_call_id: String,
        tool_name: String,
        status: ToolStatus,
    },
    ToolApprovalRequested {
        run_id: String,
        interruption_id: String,
        tool_name: String,
    },
    ToolApprovalResolved {
        run_id: String,
        interruption_id: String,
        approved: bool,
    },
    Handoff {
        run_id: String,
        from_agent: String,
        to_agent: String,
    },
    MemoryCompacted {
        run_id: String,
        before_tokens: u64,
        after_tokens: u64,
    },
    SessionPersisted {
        run_id: String,
        session_id: String,
    },
    RunCompleted {
        run_id: String,
        status: AgentStatus,
    },
    RunFailed {
        run_id: String,
        error: AgentErrorPayload,
    },
}

impl RunEvent {
    pub fn run_id(&self) -> Option<&str> {
        match self {
            Self::RunStarted { run_id, .. }
            | Self::AgentStarted { run_id, .. }
            | Self::LlmStarted { run_id, .. }
            | Self::AssistantDelta { run_id, .. }
            | Self::ToolStarted { run_id, .. }
            | Self::ToolFinished { run_id, .. }
            | Self::ToolApprovalRequested { run_id, .. }
            | Self::ToolApprovalResolved { run_id, .. }
            | Self::Handoff { run_id, .. }
            | Self::MemoryCompacted { run_id, .. }
            | Self::SessionPersisted { run_id, .. }
            | Self::RunCompleted { run_id, .. }
            | Self::RunFailed { run_id, .. } => Some(run_id),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Started,
    Success,
    Error,
    WaitResponse,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentErrorPayload {
    pub message: String,
    pub code: Option<String>,
}

impl AgentErrorPayload {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: None,
        }
    }
}
