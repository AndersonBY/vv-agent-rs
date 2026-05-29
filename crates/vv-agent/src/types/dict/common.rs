use super::*;

pub(super) fn expect_object<'a>(
    value: &'a Value,
    type_name: &str,
) -> Result<&'a serde_json::Map<String, Value>, String> {
    value
        .as_object()
        .ok_or_else(|| format!("{type_name} payload must be an object"))
}

pub(super) fn read_required_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    key: &str,
) -> Result<&'a str, String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing required string field {key:?}"))
}

pub(super) fn read_string(object: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    object.get(key).and_then(Value::as_str).map(str::to_string)
}

pub(super) fn read_optional_string(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Option<String> {
    object
        .get(key)
        .filter(|value| !value.is_null())
        .and_then(Value::as_str)
        .map(str::to_string)
}

pub(super) fn read_bool(object: &serde_json::Map<String, Value>, key: &str, default: bool) -> bool {
    object.get(key).and_then(Value::as_bool).unwrap_or(default)
}

pub(super) fn read_u32(object: &serde_json::Map<String, Value>, key: &str, default: u32) -> u32 {
    object
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(default)
}

pub(super) fn read_u64(object: &serde_json::Map<String, Value>, key: &str, default: u64) -> u64 {
    object.get(key).and_then(Value::as_u64).unwrap_or(default)
}

pub(super) fn read_u8(object: &serde_json::Map<String, Value>, key: &str, default: u8) -> u8 {
    object
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u8::try_from(value).ok())
        .unwrap_or(default)
}

pub(super) fn read_array<'a>(
    object: &'a serde_json::Map<String, Value>,
    key: &str,
) -> Option<&'a [Value]> {
    object.get(key).and_then(Value::as_array).map(Vec::as_slice)
}

pub(super) fn read_metadata(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Metadata, String> {
    match object.get(key) {
        Some(Value::Object(map)) => Ok(map.clone().into_iter().collect()),
        Some(Value::Null) | None => Ok(Metadata::new()),
        Some(_) => Err(format!("{key:?} must be an object")),
    }
}

pub(super) fn read_string_list(object: &serde_json::Map<String, Value>, key: &str) -> Vec<String> {
    object
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn insert_optional_string(
    object: &mut serde_json::Map<String, Value>,
    key: &str,
    value: &Option<String>,
) {
    if let Some(value) = value {
        object.insert(key.to_string(), Value::String(value.clone()));
    }
}

pub(super) fn insert_non_empty_optional_string(
    object: &mut serde_json::Map<String, Value>,
    key: &str,
    value: &Option<String>,
) {
    if let Some(value) = value.as_deref().filter(|value| !value.is_empty()) {
        object.insert(key.to_string(), Value::String(value.to_string()));
    }
}

pub(super) fn metadata_to_value(metadata: &Metadata) -> Value {
    Value::Object(metadata.clone().into_iter().collect())
}

pub(super) fn string_vec_to_value(items: &[String]) -> Value {
    Value::Array(items.iter().cloned().map(Value::String).collect())
}

pub(super) fn message_role_value(role: MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    }
}

pub(super) fn parse_message_role(value: &str) -> Result<MessageRole, String> {
    match value {
        "system" => Ok(MessageRole::System),
        "user" => Ok(MessageRole::User),
        "assistant" => Ok(MessageRole::Assistant),
        "tool" => Ok(MessageRole::Tool),
        other => Err(format!("unknown message role: {other}")),
    }
}

pub(super) fn tool_directive_value(directive: ToolDirective) -> &'static str {
    match directive {
        ToolDirective::Continue => "continue",
        ToolDirective::WaitUser => "wait_user",
        ToolDirective::Finish => "finish",
    }
}

pub(super) fn parse_tool_directive(value: &str) -> Result<ToolDirective, String> {
    match value {
        "continue" => Ok(ToolDirective::Continue),
        "wait_user" => Ok(ToolDirective::WaitUser),
        "finish" => Ok(ToolDirective::Finish),
        other => Err(format!("unknown tool directive: {other}")),
    }
}

pub(super) fn tool_result_status_value(status: ToolResultStatus) -> &'static str {
    match status {
        ToolResultStatus::Success => "SUCCESS",
        ToolResultStatus::Error => "ERROR",
        ToolResultStatus::WaitResponse => "WAIT_RESPONSE",
        ToolResultStatus::Running => "RUNNING",
        ToolResultStatus::PendingCompress => "PENDING_COMPRESS",
    }
}

pub(super) fn parse_tool_result_status(value: &str) -> Result<ToolResultStatus, String> {
    match value {
        "SUCCESS" => Ok(ToolResultStatus::Success),
        "ERROR" => Ok(ToolResultStatus::Error),
        "WAIT_RESPONSE" => Ok(ToolResultStatus::WaitResponse),
        "RUNNING" => Ok(ToolResultStatus::Running),
        "PENDING_COMPRESS" => Ok(ToolResultStatus::PendingCompress),
        other => Err(format!("unknown tool result status: {other}")),
    }
}

pub(super) fn parse_simple_tool_result_status(value: &str) -> Result<ToolResultStatus, String> {
    match value {
        "success" => Ok(ToolResultStatus::Success),
        "error" => Ok(ToolResultStatus::Error),
        _ => Ok(ToolResultStatus::Success),
    }
}

pub(super) fn tool_result_simple_status(status: ToolResultStatus) -> &'static str {
    match status {
        ToolResultStatus::Error => "error",
        ToolResultStatus::Success
        | ToolResultStatus::WaitResponse
        | ToolResultStatus::Running
        | ToolResultStatus::PendingCompress => "success",
    }
}

pub(super) fn no_tool_policy_value(policy: NoToolPolicy) -> &'static str {
    match policy {
        NoToolPolicy::Continue => "continue",
        NoToolPolicy::WaitUser => "wait_user",
        NoToolPolicy::Finish => "finish",
    }
}

pub(super) fn parse_no_tool_policy(value: &str) -> Result<NoToolPolicy, String> {
    match value {
        "continue" => Ok(NoToolPolicy::Continue),
        "wait_user" => Ok(NoToolPolicy::WaitUser),
        "finish" => Ok(NoToolPolicy::Finish),
        other => Err(format!("unknown no_tool_policy: {other}")),
    }
}

pub(super) fn agent_status_value(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Pending => "pending",
        AgentStatus::Running => "running",
        AgentStatus::WaitUser => "wait_user",
        AgentStatus::Completed => "completed",
        AgentStatus::Failed => "failed",
        AgentStatus::MaxCycles => "max_cycles",
    }
}

pub(super) fn parse_agent_status(value: &str) -> Result<AgentStatus, String> {
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
