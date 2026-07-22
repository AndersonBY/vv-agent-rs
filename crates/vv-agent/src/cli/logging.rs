use std::sync::Arc;

use chrono::{SecondsFormat, Utc};

use crate::events::RunEvent;
use crate::runtime::RunEventHandler;

pub(super) fn build_cli_event_handler(enabled: bool) -> Option<RunEventHandler> {
    if !enabled {
        return None;
    }
    Some(Arc::new(|event: &RunEvent| {
        let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        let line = format_cli_event_line(&now, event);
        eprintln!("{line}");
    }))
}

fn format_cli_event_line(now: &str, event: &RunEvent) -> String {
    let wire = serde_json::to_value(event).unwrap_or_default();
    let event_type = wire
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("run_event");
    format!("[{now}] [{event_type}] {wire}")
}
