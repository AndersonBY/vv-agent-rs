use crate::types::{AgentStatus, MessageRole, NoToolPolicy, ToolDirective, ToolResultStatus};

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

pub(in crate::types::dict) fn parse_no_tool_policy(value: &str) -> Result<NoToolPolicy, String> {
    match value {
        "continue" => Ok(NoToolPolicy::Continue),
        "wait_user" => Ok(NoToolPolicy::WaitUser),
        "finish" => Ok(NoToolPolicy::Finish),
        other => Err(format!("unknown no_tool_policy: {other}")),
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
        other => Err(format!("unknown agent status: {other}")),
    }
}
