use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use super::helpers::{now_iso, payload_u32, preview_text};
use super::manager::SubTaskManager;

impl SubTaskManager {
    pub(super) fn handle_session_event(
        &self,
        task_id: &str,
        event: &str,
        payload: &BTreeMap<String, Value>,
    ) {
        let mut tasks = self.tasks.lock().expect("sub-task manager poisoned");
        let Some(record) = tasks.get_mut(task_id) else {
            return;
        };
        record.updated_at = now_iso();
        match event {
            "session_run_start" => {
                if let Some(prompt) = preview_text(payload.get("prompt")) {
                    record.task_title = prompt.clone();
                    record.recent_activity = Some(prompt);
                }
            }
            "cycle_started" => {
                if let Some(cycle_index) = payload_u32(payload, "cycle") {
                    record.current_cycle_index = Some(cycle_index);
                    record.latest_cycle = Some(json!({
                        "cycle_index": cycle_index,
                        "status": "processing",
                    }));
                }
            }
            "cycle_llm_response" => {
                if let Some(cycle_index) = payload_u32(payload, "cycle") {
                    record.current_cycle_index = Some(cycle_index);
                }
                let assistant_preview = preview_text(
                    payload
                        .get("assistant_preview")
                        .or_else(|| payload.get("assistant_message")),
                );
                let mut latest_cycle = Map::new();
                latest_cycle.insert(
                    "status".to_string(),
                    Value::String("processing".to_string()),
                );
                if let Some(cycle_index) = record.current_cycle_index {
                    latest_cycle.insert("cycle_index".to_string(), Value::from(cycle_index));
                }
                if let Some(assistant_preview) = assistant_preview {
                    latest_cycle.insert(
                        "assistant_preview".to_string(),
                        Value::String(assistant_preview.clone()),
                    );
                    record.recent_activity = Some(assistant_preview);
                }
                record.latest_cycle = Some(Value::Object(latest_cycle));
            }
            "tool_result" => {
                let tool_status = preview_text(payload.get("status"));
                record.latest_tool_call = Some(json!({
                    "tool_call_id": payload.get("tool_call_id").cloned().unwrap_or(Value::Null),
                    "name": payload.get("tool_name").cloned().unwrap_or(Value::Null),
                    "status": tool_status,
                }));
                if record.recent_activity.is_none() {
                    record.recent_activity = preview_text(payload.get("tool_name"));
                }
            }
            "run_completed" => {
                record.mark_terminal_state(
                    "completed",
                    preview_text(payload.get("final_answer")).as_deref(),
                );
            }
            "run_wait_user" => {
                record.mark_terminal_state(
                    "wait_user",
                    preview_text(payload.get("wait_reason")).as_deref(),
                );
            }
            "run_max_cycles" => {
                let detail =
                    preview_text(payload.get("final_answer").or_else(|| payload.get("error")));
                record.mark_terminal_state("max_cycles", detail.as_deref());
            }
            "cycle_failed" => {
                record.mark_terminal_state("failed", preview_text(payload.get("error")).as_deref());
            }
            "session_run_end" => {
                if let Some(status) = preview_text(payload.get("status")) {
                    record.set_latest_cycle_status(&status);
                }
                let detail = preview_text(payload.get("final_answer"))
                    .or_else(|| preview_text(payload.get("wait_reason")))
                    .or_else(|| preview_text(payload.get("error")));
                if let Some(detail) = detail {
                    record.recent_activity = Some(detail);
                }
            }
            _ => {}
        }
    }
}
