use std::collections::BTreeMap;
use std::env;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::Value;

pub(crate) type RecordedEvents = Arc<Mutex<Vec<(String, BTreeMap<String, Value>)>>>;
pub(crate) type RecordingListener =
    Arc<dyn Fn(&str, &BTreeMap<String, Value>) + Send + Sync + 'static>;

pub(crate) fn recorded_events() -> RecordedEvents {
    Arc::new(Mutex::new(Vec::new()))
}

pub(crate) fn recording_listener(events: &RecordedEvents) -> RecordingListener {
    let events = Arc::clone(events);
    Arc::new(move |event, payload| {
        events
            .lock()
            .expect("events lock")
            .push((event.to_string(), payload.clone()));
    })
}

pub(crate) fn summarize_events(events: &[(String, BTreeMap<String, Value>)]) -> String {
    if events.is_empty() {
        return "no session events captured".to_string();
    }

    let mut lines = Vec::new();
    for (event, payload) in events.iter().rev().take(30).rev() {
        let metadata = payload.get("metadata").and_then(Value::as_object);
        let mut summary = BTreeMap::new();
        summary.insert("event".to_string(), Value::String(event.clone()));
        for key in [
            "tool_name",
            "status",
            "session_id",
            "queued_to_running_session",
            "final_answer",
            "wait_reason",
            "error",
            "output",
            "content_preview",
        ] {
            if let Some(value) = payload.get(key).cloned() {
                summary.insert(key.to_string(), value);
            }
        }
        if let Some(metadata) = metadata {
            if let Some(value) = metadata.get("transitioned_to_background").cloned() {
                summary.insert("transitioned_to_background".to_string(), value);
            }
            if let Some(value) = metadata.get("session_id").cloned() {
                summary.insert("metadata_session_id".to_string(), value);
            }
        }
        lines.push(Value::Object(summary.into_iter().collect()).to_string());
    }
    lines.join("\n")
}

pub(crate) fn live_enabled() -> bool {
    env::var("VV_AGENT_RUN_LIVE_TESTS")
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

pub(crate) fn live_settings_path() -> PathBuf {
    env::var("VV_AGENT_LIVE_SETTINGS_JSON")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
                "../../../third_party_service/vv-llm-rs/crates/vv-llm/tests/fixtures/dev_settings.json",
            )
        })
}
