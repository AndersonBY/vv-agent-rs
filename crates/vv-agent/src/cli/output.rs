use serde_json::{json, Value};

use crate::config::ResolvedModelConfig;
use crate::types::{AgentResult, AgentStatus};

pub fn result_payload(result: &AgentResult, resolved: &ResolvedModelConfig) -> Value {
    json!({
        "status": status_value(result.status),
        "final_answer": result.final_answer,
        "wait_reason": result.wait_reason,
        "error": result.error,
        "cycles": result.cycles.len(),
        "todo_list": result.todo_list(),
        "resolved": {
            "backend": resolved.backend,
            "selected_model": resolved.selected_model,
            "model_id": resolved.model_id,
            "endpoint": resolved.endpoint().map(|endpoint| endpoint.endpoint_id.clone()),
        },
    })
}

fn status_value(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Pending => "pending",
        AgentStatus::Running => "running",
        AgentStatus::WaitUser => "wait_user",
        AgentStatus::Completed => "completed",
        AgentStatus::Failed => "failed",
        AgentStatus::MaxCycles => "max_cycles",
    }
}
