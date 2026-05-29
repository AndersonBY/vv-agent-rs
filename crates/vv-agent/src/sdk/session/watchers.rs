use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::runtime::background_sessions::{
    background_session_manager, BackgroundSessionSubscription,
};

use super::events::emit_session_event;
use super::handles::SessionSteeringHandle;
use super::state::{SessionEventHandler, SessionListenerId};

pub(super) fn sync_background_command_watchers(
    subscriptions: &Arc<Mutex<BTreeMap<String, BackgroundSessionSubscription>>>,
    listeners: &Arc<Mutex<BTreeMap<SessionListenerId, SessionEventHandler>>>,
    steering_handle: &SessionSteeringHandle,
    event: &str,
    payload: &BTreeMap<String, Value>,
) {
    if event != "tool_result" {
        return;
    }
    let tool_name = payload
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if tool_name != "bash" && tool_name != "check_background_command" {
        return;
    }
    let metadata = payload.get("metadata").and_then(Value::as_object);
    let session_id = metadata
        .and_then(|metadata| metadata.get("session_id"))
        .or_else(|| payload.get("session_id"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if session_id.is_empty() {
        return;
    }
    let status = metadata
        .and_then(|metadata| metadata.get("status"))
        .or_else(|| payload.get("status"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();

    if status == "running" {
        let mut subscriptions = subscriptions
            .lock()
            .expect("background command subscriptions lock");
        if subscriptions.contains_key(&session_id) {
            return;
        }
        let listener_session_id = session_id.clone();
        let listener_events = Arc::clone(listeners);
        let listener_steering = steering_handle.clone();
        let subscription = background_session_manager().subscribe(
            &session_id,
            Arc::new(move |payload| {
                handle_background_command_terminal(
                    &listener_events,
                    &listener_steering,
                    &listener_session_id,
                    payload,
                );
            }),
        );
        subscriptions.insert(session_id, subscription);
        return;
    }

    if matches!(
        status.as_str(),
        "completed" | "failed" | "timeout" | "missing"
    ) {
        subscriptions
            .lock()
            .expect("background command subscriptions lock")
            .remove(&session_id);
    }
}

fn handle_background_command_terminal(
    listeners: &Arc<Mutex<BTreeMap<SessionListenerId, SessionEventHandler>>>,
    steering_handle: &SessionSteeringHandle,
    session_id: &str,
    payload: &Value,
) {
    let notification_message = build_background_command_notification(payload);
    let queued_to_running_session = steering_handle.steer(notification_message.clone()).is_ok();
    let mut event_payload = payload
        .as_object()
        .map(|object| {
            object
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    event_payload.insert(
        "session_id".to_string(),
        Value::String(session_id.to_string()),
    );
    event_payload.insert(
        "notification_message".to_string(),
        Value::String(notification_message),
    );
    event_payload.insert(
        "queued_to_running_session".to_string(),
        Value::Bool(queued_to_running_session),
    );

    let status = event_payload
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("terminal")
        .trim()
        .to_ascii_lowercase();
    emit_session_event(
        listeners,
        &format!("background_command_{status}"),
        event_payload.clone(),
    );
    emit_session_event(listeners, "background_command_terminal", event_payload);
}

fn build_background_command_notification(payload: &Value) -> String {
    let status = payload
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let status_text = match status.as_str() {
        "completed" => "completed",
        "failed" => "failed",
        "timeout" => "timed out",
        _ if !status.is_empty() => status.as_str(),
        _ => "updated",
    };
    let session_id = payload
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    let command = payload
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    let output = payload
        .get("output")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    let exit_code = payload.get("exit_code").cloned().unwrap_or(Value::Null);
    let mut summary = if output.is_empty() {
        format!("exit_code={exit_code}")
    } else {
        output.to_string()
    };
    if summary.len() > 500 {
        summary.truncate(497);
        summary = format!("{}...", summary.trim_end());
    }

    let mut lines = vec![format!(
        "System notification: background command {session_id} {status_text}."
    )];
    if !command.is_empty() {
        lines.push(format!("Command: {command}"));
    }
    if !summary.is_empty() {
        lines.push(format!("Summary: {summary}"));
    }
    lines.join("\n")
}
