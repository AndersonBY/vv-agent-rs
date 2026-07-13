use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub trait TraceSink: Send + Sync {
    fn on_span_start(&self, span: &Span);
    fn on_span_end(&self, span: &Span);

    fn flush(&self) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Span {
    pub name: String,
    pub trace_id: String,
    pub span_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub started_at: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<f64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, Value>,
}

impl Span {
    pub fn new(trace_id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            trace_id: trace_id.into(),
            span_id: format!("span_{}", uuid::Uuid::new_v4().simple()),
            parent_id: None,
            started_at: timestamp_seconds(),
            ended_at: None,
            metadata: BTreeMap::new(),
        }
    }

    pub fn with_parent_id(mut self, parent_id: impl Into<String>) -> Self {
        self.parent_id = Some(parent_id.into());
        self
    }

    pub fn with_metadata(mut self, key: impl Into<String>, value: Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }

    pub fn finish(mut self) -> Self {
        self.ended_at = Some(timestamp_seconds());
        self
    }
}

pub struct JsonlTraceExporter {
    file: Mutex<File>,
}

impl JsonlTraceExporter {
    pub fn new(path: impl AsRef<Path>) -> Result<Self, String> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|error| error.to_string())?;
        Ok(Self {
            file: Mutex::new(file),
        })
    }

    fn write_event(&self, event: &str, span: &Span) {
        if let Ok(mut file) = self.file.lock() {
            let _ = writeln!(
                file,
                "{}",
                json!({
                    "event": event,
                    "timestamp": timestamp_seconds(),
                    "span": span,
                })
            );
        }
    }
}

impl TraceSink for JsonlTraceExporter {
    fn on_span_start(&self, span: &Span) {
        self.write_event("span_start", span);
    }

    fn on_span_end(&self, span: &Span) {
        self.write_event("span_end", span);
    }

    fn flush(&self) -> Result<(), String> {
        self.file
            .lock()
            .map_err(|_| "trace exporter lock poisoned".to_string())?
            .flush()
            .map_err(|error| error.to_string())
    }
}

fn timestamp_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or_default()
}
