use std::any::Any;
use std::collections::BTreeMap;

use chrono::{SecondsFormat, Utc};
use serde_json::Value;

use crate::types::AgentStatus;

pub(super) fn status_label(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Pending => "pending",
        AgentStatus::Running => "running",
        AgentStatus::WaitUser => "wait_user",
        AgentStatus::Completed => "completed",
        AgentStatus::Failed => "failed",
        AgentStatus::MaxCycles => "max_cycles",
    }
}

pub(super) fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Micros, false)
}

pub(super) fn panic_payload_to_string(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "sub-task runner panicked".to_string()
}

pub(super) fn payload_u32(payload: &BTreeMap<String, Value>, key: &str) -> Option<u32> {
    payload
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
}

pub(super) fn preview_text(value: Option<&Value>) -> Option<String> {
    const PREVIEW_LIMIT: usize = 240;
    let text = match value? {
        Value::Null => return None,
        Value::String(value) => value.clone(),
        other => other.to_string(),
    };
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if text.len() <= PREVIEW_LIMIT {
        return Some(text.to_string());
    }
    let mut truncated = text
        .chars()
        .take(PREVIEW_LIMIT.saturating_sub(3))
        .collect::<String>();
    truncated = truncated.trim_end().to_string();
    truncated.push_str("...");
    Some(truncated)
}

pub(super) fn is_internal_workspace_file(path: &str) -> bool {
    let normalized = path.trim().trim_matches('/');
    normalized.is_empty()
        || normalized
            .split('/')
            .any(|part| part.is_empty() || part.starts_with('.'))
}
