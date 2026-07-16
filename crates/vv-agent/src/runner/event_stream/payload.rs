use std::collections::BTreeMap;

use serde_json::Value;

use crate::types::{AgentStatus, CompletionReason};

pub(super) fn agent_status(payload: &BTreeMap<String, Value>) -> AgentStatus {
    match payload
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("completed")
        .to_ascii_lowercase()
        .as_str()
    {
        "pending" => AgentStatus::Pending,
        "running" => AgentStatus::Running,
        "wait_user" | "wait_response" => AgentStatus::WaitUser,
        "failed" | "error" => AgentStatus::Failed,
        "max_cycles" => AgentStatus::MaxCycles,
        _ => AgentStatus::Completed,
    }
}

pub(super) fn completion_reason_from_payload(
    payload: &BTreeMap<String, Value>,
    fallback: Option<CompletionReason>,
) -> Option<CompletionReason> {
    payload
        .get("completion_reason")
        .and_then(Value::as_str)
        .and_then(CompletionReason::parse)
        .or(fallback)
}
