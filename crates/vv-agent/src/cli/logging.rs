use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use chrono::{SecondsFormat, Utc};
use serde_json::Value;

use crate::runtime::RuntimeLogCallback;

pub(super) fn build_cli_log_handler(enabled: bool) -> Option<Arc<Mutex<Box<RuntimeLogCallback>>>> {
    if !enabled {
        return None;
    }
    let handler: Box<RuntimeLogCallback> = Box::new(|event, payload| {
        let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        let line = format_cli_log_line(&now, event, payload);
        eprintln!("{line}");
    });
    Some(Arc::new(Mutex::new(handler)))
}

fn format_cli_log_line(now: &str, event: &str, payload: &BTreeMap<String, Value>) -> String {
    match event {
        "run_started" => format!(
            "[{now}] [run] start task={} model={} max_cycles={}",
            payload_text(payload, "task_id"),
            payload_text(payload, "model"),
            payload_text(payload, "max_cycles")
        ),
        "cycle_started" => format!(
            "[{now}] [cycle {}] start messages={}",
            payload_text(payload, "cycle"),
            payload_text(payload, "message_count")
        ),
        "cycle_llm_response" => format!(
            "[{now}] [cycle {}] llm tool_calls={} assistant={}",
            payload_text(payload, "cycle"),
            payload_text(payload, "tool_call_names"),
            payload_text(payload, "assistant_preview")
        ),
        "tool_result" => format!(
            "[{now}] [cycle {}] tool={} status={} directive={} preview={}",
            payload_text(payload, "cycle"),
            payload_text(payload, "tool_name"),
            payload_text(payload, "status"),
            payload_text(payload, "directive"),
            payload_text(payload, "content_preview")
        ),
        "run_completed" => format!(
            "[{now}] [run] completed: {}",
            payload_text(payload, "final_answer")
        ),
        "run_wait_user" => format!(
            "[{now}] [run] wait_user: {}",
            payload_text(payload, "wait_reason")
        ),
        "run_max_cycles" => format!("[{now}] [run] max_cycles reached"),
        "cycle_failed" => format!(
            "[{now}] [cycle {}] failed: {}",
            payload_text(payload, "cycle"),
            payload_text(payload, "error")
        ),
        other => format!(
            "[{now}] [{other}] {}",
            Value::Object(payload.clone().into_iter().collect())
        ),
    }
}

fn payload_text(payload: &BTreeMap<String, Value>, key: &str) -> String {
    payload
        .get(key)
        .map(|value| match value {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        })
        .unwrap_or_default()
}
