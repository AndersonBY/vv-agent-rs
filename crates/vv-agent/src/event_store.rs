use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use crate::checkpoint::EventCursor;
use crate::events::RunEvent;

pub trait RunEventStore: Send + Sync {
    fn append(&self, event: &RunEvent) -> Result<(), EventStoreError>;
    fn replay(&self, query: RunEventReplayQuery) -> Result<RunEventIter, EventStoreError>;

    fn append_once(
        &self,
        _event_id: &str,
        _payload_digest: &str,
        _event: &RunEvent,
    ) -> Result<Option<EventCursor>, EventStoreError> {
        Ok(None)
    }
}

pub type RunEventIter = Box<dyn Iterator<Item = Result<RunEvent, EventStoreError>> + Send>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunEventReplayQuery {
    run_id: String,
    include_children: bool,
}

impl RunEventReplayQuery {
    pub fn run(run_id: impl Into<String>) -> Self {
        Self {
            run_id: run_id.into(),
            include_children: true,
        }
    }

    pub fn include_children(mut self, include_children: bool) -> Self {
        self.include_children = include_children;
        self
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub fn should_include_children(&self) -> bool {
        self.include_children
    }
}

#[derive(Debug, Clone)]
pub struct JsonlRunEventStore {
    path: PathBuf,
}

impl JsonlRunEventStore {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }
}

impl RunEventStore for JsonlRunEventStore {
    fn append(&self, event: &RunEvent) -> Result<(), EventStoreError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(EventStoreError::io)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(EventStoreError::io)?;
        let line = serde_json::to_string(event).map_err(EventStoreError::json)?;
        writeln!(file, "{line}").map_err(EventStoreError::io)?;
        Ok(())
    }

    fn replay(&self, query: RunEventReplayQuery) -> Result<RunEventIter, EventStoreError> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Box::new(std::iter::empty()));
            }
            Err(error) => return Err(EventStoreError::io(error)),
        };
        let mut lines = BufReader::new(file).lines().enumerate();
        let mut stopped = false;

        Ok(Box::new(std::iter::from_fn(move || {
            if stopped {
                return None;
            }

            loop {
                let Some((line_index, line)) = lines.next() else {
                    stopped = true;
                    return None;
                };
                let line_number = line_index + 1;
                let line = match line {
                    Ok(line) => line,
                    Err(error) => {
                        stopped = true;
                        return Some(Err(EventStoreError::io(error)));
                    }
                };
                let event: RunEvent = match serde_json::from_str(&line) {
                    Ok(event) => event,
                    Err(_) => {
                        stopped = true;
                        return Some(Err(EventStoreError::corrupt_line(line_number)));
                    }
                };
                let include = event.run_id() == query.run_id()
                    || (query.should_include_children()
                        && event.parent_run_id() == Some(query.run_id()));
                if include {
                    return Some(Ok(event));
                }
            }
        })))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventStoreError {
    code: &'static str,
    message: String,
    line_number: Option<usize>,
}

impl EventStoreError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            line_number: None,
        }
    }

    fn io(error: std::io::Error) -> Self {
        Self {
            code: "event_store_io_error",
            message: error.to_string(),
            line_number: None,
        }
    }

    fn json(error: serde_json::Error) -> Self {
        Self {
            code: "event_store_serialization_error",
            message: error.to_string(),
            line_number: None,
        }
    }

    fn corrupt_line(line_number: usize) -> Self {
        Self {
            code: "event_store_corrupt_line",
            message: format!("event store corrupt line {line_number}"),
            line_number: Some(line_number),
        }
    }

    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn line_number(&self) -> Option<usize> {
        self.line_number
    }
}

impl fmt::Display for EventStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for EventStoreError {}
