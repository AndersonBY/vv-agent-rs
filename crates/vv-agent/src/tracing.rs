use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::json;

pub trait TraceSink: Send + Sync {
    fn on_span_start(&self, span: &Span);
    fn on_span_end(&self, span: &Span);

    fn flush(&self) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub trace_id: String,
    pub span_id: String,
    pub name: String,
    pub run_id: String,
    pub agent_name: Option<String>,
}

impl Span {
    pub fn new(
        run_id: impl Into<String>,
        name: impl Into<String>,
        agent_name: Option<String>,
    ) -> Self {
        let run_id = run_id.into();
        let name = name.into();
        Self {
            trace_id: run_id.clone(),
            span_id: format!("{}_{}", name, timestamp_millis()),
            name,
            run_id,
            agent_name,
        }
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
                    "timestamp_ms": timestamp_millis(),
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

fn timestamp_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}
