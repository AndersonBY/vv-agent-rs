use crate::types::{
    AgentStatus, CompletionReason, MessageRole, NoToolPolicy, ToolDirective, ToolResultStatus,
};

pub(in crate::types::dict) fn message_role_value(role: MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    }
}

pub(in crate::types::dict) fn parse_message_role(value: &str) -> Result<MessageRole, String> {
    match value {
        "system" => Ok(MessageRole::System),
        "user" => Ok(MessageRole::User),
        "assistant" => Ok(MessageRole::Assistant),
        "tool" => Ok(MessageRole::Tool),
        other => Err(format!("unknown message role: {other}")),
    }
}

pub(in crate::types::dict) fn tool_directive_value(directive: ToolDirective) -> &'static str {
    match directive {
        ToolDirective::Continue => "continue",
        ToolDirective::WaitUser => "wait_user",
        ToolDirective::Finish => "finish",
    }
}

pub(in crate::types::dict) fn parse_tool_directive(value: &str) -> Result<ToolDirective, String> {
    match value {
        "continue" => Ok(ToolDirective::Continue),
        "wait_user" => Ok(ToolDirective::WaitUser),
        "finish" => Ok(ToolDirective::Finish),
        other => Err(format!("unknown tool directive: {other}")),
    }
}

pub(in crate::types::dict) fn tool_result_status_value(status: ToolResultStatus) -> &'static str {
    match status {
        ToolResultStatus::Success => "SUCCESS",
        ToolResultStatus::Error => "ERROR",
        ToolResultStatus::WaitResponse => "WAIT_RESPONSE",
        ToolResultStatus::Running => "RUNNING",
        ToolResultStatus::PendingCompress => "PENDING_COMPRESS",
    }
}

pub(in crate::types::dict) fn parse_tool_result_status(
    value: &str,
) -> Result<ToolResultStatus, String> {
    match value {
        "SUCCESS" => Ok(ToolResultStatus::Success),
        "ERROR" => Ok(ToolResultStatus::Error),
        "WAIT_RESPONSE" => Ok(ToolResultStatus::WaitResponse),
        "RUNNING" => Ok(ToolResultStatus::Running),
        "PENDING_COMPRESS" => Ok(ToolResultStatus::PendingCompress),
        other => Err(format!("unknown tool result status: {other}")),
    }
}

pub(in crate::types::dict) fn parse_simple_tool_result_status(
    value: &str,
) -> Result<ToolResultStatus, String> {
    match value {
        "success" => Ok(ToolResultStatus::Success),
        "error" => Ok(ToolResultStatus::Error),
        _ => Ok(ToolResultStatus::Success),
    }
}

pub(in crate::types::dict) fn tool_result_simple_status(status: ToolResultStatus) -> &'static str {
    match status {
        ToolResultStatus::Error => "error",
        ToolResultStatus::Success
        | ToolResultStatus::WaitResponse
        | ToolResultStatus::Running
        | ToolResultStatus::PendingCompress => "success",
    }
}

pub(in crate::types::dict) fn no_tool_policy_value(policy: NoToolPolicy) -> &'static str {
    match policy {
        NoToolPolicy::Continue => "continue",
        NoToolPolicy::WaitUser => "wait_user",
        NoToolPolicy::Finish => "finish",
    }
}

pub(in crate::types::dict) fn agent_status_value(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Pending => "pending",
        AgentStatus::Running => "running",
        AgentStatus::WaitUser => "wait_user",
        AgentStatus::Completed => "completed",
        AgentStatus::Failed => "failed",
        AgentStatus::MaxCycles => "max_cycles",
        AgentStatus::ReconciliationRequired => "reconciliation_required",
    }
}

pub(in crate::types::dict) fn parse_agent_status(value: &str) -> Result<AgentStatus, String> {
    match value {
        "pending" => Ok(AgentStatus::Pending),
        "running" => Ok(AgentStatus::Running),
        "wait_user" => Ok(AgentStatus::WaitUser),
        "completed" => Ok(AgentStatus::Completed),
        "failed" => Ok(AgentStatus::Failed),
        "max_cycles" => Ok(AgentStatus::MaxCycles),
        "reconciliation_required" => Ok(AgentStatus::ReconciliationRequired),
        other => Err(format!("unknown agent status: {other}")),
    }
}

pub(in crate::types::dict) fn completion_reason_value(reason: CompletionReason) -> &'static str {
    match reason {
        CompletionReason::ToolFinish => "tool_finish",
        CompletionReason::NoToolFinish => "no_tool_finish",
        CompletionReason::StopOnFirstTool => "stop_on_first_tool",
        CompletionReason::StopAtToolName => "stop_at_tool_name",
        CompletionReason::WaitUser => "wait_user",
        CompletionReason::MaxCycles => "max_cycles",
        CompletionReason::Cancelled => "cancelled",
        CompletionReason::Failed => "failed",
        CompletionReason::BudgetExhausted => "budget_exhausted",
    }
}

pub(in crate::types::dict) fn parse_completion_reason(
    value: &str,
) -> Result<CompletionReason, String> {
    match value {
        "tool_finish" => Ok(CompletionReason::ToolFinish),
        "no_tool_finish" => Ok(CompletionReason::NoToolFinish),
        "stop_on_first_tool" => Ok(CompletionReason::StopOnFirstTool),
        "stop_at_tool_name" => Ok(CompletionReason::StopAtToolName),
        "wait_user" => Ok(CompletionReason::WaitUser),
        "max_cycles" => Ok(CompletionReason::MaxCycles),
        "cancelled" => Ok(CompletionReason::Cancelled),
        "failed" => Ok(CompletionReason::Failed),
        "budget_exhausted" => Ok(CompletionReason::BudgetExhausted),
        other => Err(format!("unknown completion reason: {other}")),
    }
}
