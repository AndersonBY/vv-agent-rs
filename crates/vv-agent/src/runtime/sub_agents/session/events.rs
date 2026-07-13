use std::collections::BTreeMap;

use serde_json::Value;

use crate::runtime::sub_agents::events::{
    agent_status_value, emit_parent_sub_agent_event, emit_sub_agent_session_event,
    enrich_sub_agent_payload,
};
use crate::runtime::sub_task_manager::SubTaskTurnSnapshot;
use crate::types::AgentResult;

use super::RuntimeSubAgentSession;

impl RuntimeSubAgentSession {
    pub(in crate::runtime::sub_agents) fn emit(
        &self,
        event: &str,
        payload: BTreeMap<String, Value>,
    ) {
        emit_sub_agent_session_event(&self.listeners, event, &payload);
        let enriched =
            enrich_sub_agent_payload(&payload, &self.task_id, &self.session_id, &self.agent_name);
        let current_turn_event_handler =
            SubTaskTurnSnapshot::current_event_handler().and_then(|handler| handler);
        emit_parent_sub_agent_event(
            &self.parent_log_handler,
            &current_turn_event_handler,
            &format!("sub_agent_{event}"),
            enriched,
        );
    }

    pub(super) fn emit_session_run_end(&self, result: &AgentResult) {
        self.emit(
            "session_run_end",
            BTreeMap::from([
                (
                    "status".to_string(),
                    Value::String(agent_status_value(result.status).to_string()),
                ),
                (
                    "cycles".to_string(),
                    Value::from(result.cycles.len() as u64),
                ),
                (
                    "final_answer".to_string(),
                    result
                        .final_answer
                        .clone()
                        .map(Value::String)
                        .unwrap_or(Value::Null),
                ),
                (
                    "wait_reason".to_string(),
                    result
                        .wait_reason
                        .clone()
                        .map(Value::String)
                        .unwrap_or(Value::Null),
                ),
                (
                    "error".to_string(),
                    result
                        .error
                        .clone()
                        .map(Value::String)
                        .unwrap_or(Value::Null),
                ),
            ]),
        );
    }
}
