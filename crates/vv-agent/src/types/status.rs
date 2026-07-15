use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Pending,
    Running,
    WaitUser,
    Completed,
    Failed,
    MaxCycles,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CompletionReason {
    ToolFinish,
    NoToolFinish,
    StopOnFirstTool,
    StopAtToolName,
    WaitUser,
    MaxCycles,
    Cancelled,
    Failed,
    BudgetExhausted,
}

impl CompletionReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ToolFinish => "tool_finish",
            Self::NoToolFinish => "no_tool_finish",
            Self::StopOnFirstTool => "stop_on_first_tool",
            Self::StopAtToolName => "stop_at_tool_name",
            Self::WaitUser => "wait_user",
            Self::MaxCycles => "max_cycles",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
            Self::BudgetExhausted => "budget_exhausted",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "tool_finish" => Some(Self::ToolFinish),
            "no_tool_finish" => Some(Self::NoToolFinish),
            "stop_on_first_tool" => Some(Self::StopOnFirstTool),
            "stop_at_tool_name" => Some(Self::StopAtToolName),
            "wait_user" => Some(Self::WaitUser),
            "max_cycles" => Some(Self::MaxCycles),
            "cancelled" => Some(Self::Cancelled),
            "failed" => Some(Self::Failed),
            "budget_exhausted" => Some(Self::BudgetExhausted),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolDirective {
    Continue,
    WaitUser,
    Finish,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ToolResultStatus {
    Success,
    Error,
    WaitResponse,
    Running,
    PendingCompress,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CycleStatus {
    Pending,
    Processing,
    Completed,
    WaitResponse,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NoToolPolicy {
    #[default]
    Continue,
    WaitUser,
    Finish,
}
