use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::runtime::sub_agent_sessions::SubAgentSessionListener;
use crate::runtime::{RuntimeEventHandler, RuntimeLogHandler};
use crate::types::AgentStatus;

pub(super) fn emit_sub_agent_session_event(
    listeners: &Arc<Mutex<BTreeMap<u64, SubAgentSessionListener>>>,
    event: &str,
    payload: &BTreeMap<String, Value>,
) {
    let listeners = listeners
        .lock()
        .expect("sub-agent session listeners poisoned")
        .values()
        .cloned()
        .collect::<Vec<_>>();
    for listener in listeners {
        listener(event, payload);
    }
}

pub(super) fn enrich_sub_agent_payload(
    payload: &BTreeMap<String, Value>,
    task_id: &str,
    session_id: &str,
    sub_agent_name: &str,
) -> BTreeMap<String, Value> {
    let mut enriched = payload.clone();
    enriched
        .entry("task_id".to_string())
        .or_insert_with(|| Value::String(task_id.to_string()));
    enriched
        .entry("session_id".to_string())
        .or_insert_with(|| Value::String(session_id.to_string()));
    enriched
        .entry("sub_agent_name".to_string())
        .or_insert_with(|| Value::String(sub_agent_name.to_string()));
    enriched
}

pub(super) fn emit_parent_sub_agent_event(
    parent_log_handler: &Option<RuntimeLogHandler>,
    parent_event_handler: &Option<RuntimeEventHandler>,
    event: &str,
    payload: BTreeMap<String, Value>,
) {
    if let Some(handler) = parent_log_handler {
        if let Ok(mut handler) = handler.lock() {
            handler(event, &payload);
        }
    }
    if let Some(handler) = parent_event_handler {
        handler(event, &payload);
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
