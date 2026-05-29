use std::collections::BTreeMap;

use serde_json::Value;

use crate::config::ResolvedModelConfig;
use crate::types::{AgentResult, AgentStatus};

#[derive(Debug, Clone)]
pub struct AgentRun {
    pub agent_name: String,
    pub result: AgentResult,
    pub resolved: ResolvedModelConfig,
}

impl AgentRun {
    pub fn to_dict(&self) -> BTreeMap<String, Value> {
        let mut payload = BTreeMap::new();
        payload.insert("agent".to_string(), Value::String(self.agent_name.clone()));
        payload.insert(
            "status".to_string(),
            Value::String(agent_status_value(self.result.status).to_string()),
        );
        payload.insert(
            "final_answer".to_string(),
            self.result
                .final_answer
                .clone()
                .map(Value::String)
                .unwrap_or(Value::Null),
        );
        payload.insert(
            "wait_reason".to_string(),
            self.result
                .wait_reason
                .clone()
                .map(Value::String)
                .unwrap_or(Value::Null),
        );
        payload.insert(
            "error".to_string(),
            self.result
                .error
                .clone()
                .map(Value::String)
                .unwrap_or(Value::Null),
        );
        payload.insert(
            "cycles".to_string(),
            Value::Number(serde_json::Number::from(self.result.cycles.len() as u64)),
        );
        payload.insert(
            "todo_list".to_string(),
            Value::Array(self.result.todo_list()),
        );
        payload.insert(
            "token_usage".to_string(),
            serde_json::to_value(&self.result.token_usage).unwrap_or(Value::Null),
        );
        let mut resolved = serde_json::Map::new();
        resolved.insert(
            "backend".to_string(),
            Value::String(self.resolved.backend.clone()),
        );
        resolved.insert(
            "selected_model".to_string(),
            Value::String(self.resolved.selected_model.clone()),
        );
        resolved.insert(
            "model_id".to_string(),
            Value::String(self.resolved.model_id.clone()),
        );
        resolved.insert(
            "endpoint".to_string(),
            self.resolved
                .endpoint()
                .map(|endpoint| Value::String(endpoint.endpoint_id.clone()))
                .unwrap_or(Value::Null),
        );
        payload.insert("resolved".to_string(), Value::Object(resolved));
        payload
    }
}

pub(crate) fn agent_status_value(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Pending => "pending",
        AgentStatus::Running => "running",
        AgentStatus::WaitUser => "wait_user",
        AgentStatus::Completed => "completed",
        AgentStatus::Failed => "failed",
        AgentStatus::MaxCycles => "max_cycles",
    }
}
