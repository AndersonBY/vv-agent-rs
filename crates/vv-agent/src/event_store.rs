use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use crate::events::RunEvent;

pub trait RunEventStore: Send + Sync {
    fn append(&self, event: &RunEvent) -> Result<(), EventStoreError>;
    fn replay(&self, query: RunEventReplayQuery) -> Result<RunEventIter, EventStoreError>;
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
            include_children: false,
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
        if !self.path.exists() {
            return Ok(Box::new(std::iter::empty()));
        }

        let file = File::open(&self.path).map_err(EventStoreError::io)?;
        let reader = BufReader::new(file);
        let mut events = Vec::new();
        for line in reader.lines() {
            let line = line.map_err(EventStoreError::io)?;
            let event: RunEvent = serde_json::from_str(&line).map_err(EventStoreError::json)?;
            let include = event.run_id() == query.run_id()
                || (query.should_include_children()
                    && event.parent_run_id() == Some(query.run_id()));
            if include {
                events.push(event);
            }
        }

        Ok(Box::new(events.into_iter().map(Ok)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventStoreError {
    message: String,
}

impl EventStoreError {
    fn io(error: std::io::Error) -> Self {
        Self {
            message: error.to_string(),
        }
    }

    fn json(error: serde_json::Error) -> Self {
        Self {
            message: error.to_string(),
        }
    }
}

impl fmt::Display for EventStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for EventStoreError {}
