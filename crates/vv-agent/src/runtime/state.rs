use std::collections::BTreeMap;
use std::io::{Error, ErrorKind, Result};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::types::{AgentStatus, CycleRecord, Message, Metadata};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub task_id: String,
    pub cycle_index: u32,
    pub status: AgentStatus,
    pub messages: Vec<Message>,
    pub cycles: Vec<CycleRecord>,
    pub shared_state: Metadata,
}

pub trait StateStore: Send + Sync {
    fn save_checkpoint(&self, checkpoint: Checkpoint) -> Result<()>;
    fn load_checkpoint(&self, task_id: &str) -> Result<Option<Checkpoint>>;
    fn delete_checkpoint(&self, task_id: &str) -> Result<()>;
    fn list_checkpoints(&self) -> Result<Vec<String>>;
}

#[derive(Debug, Clone, Default)]
pub struct InMemoryStateStore {
    checkpoints: Arc<Mutex<BTreeMap<String, Checkpoint>>>,
}

impl InMemoryStateStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl StateStore for InMemoryStateStore {
    fn save_checkpoint(&self, checkpoint: Checkpoint) -> Result<()> {
        self.checkpoints
            .lock()
            .map_err(|_| poisoned("state store"))?
            .insert(checkpoint.task_id.clone(), checkpoint);
        Ok(())
    }

    fn load_checkpoint(&self, task_id: &str) -> Result<Option<Checkpoint>> {
        Ok(self
            .checkpoints
            .lock()
            .map_err(|_| poisoned("state store"))?
            .get(task_id)
            .cloned())
    }

    fn delete_checkpoint(&self, task_id: &str) -> Result<()> {
        self.checkpoints
            .lock()
            .map_err(|_| poisoned("state store"))?
            .remove(task_id);
        Ok(())
    }

    fn list_checkpoints(&self) -> Result<Vec<String>> {
        Ok(self
            .checkpoints
            .lock()
            .map_err(|_| poisoned("state store"))?
            .keys()
            .cloned()
            .collect())
    }
}

pub(crate) fn checkpoint_status_value(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Pending => "pending",
        AgentStatus::Running => "running",
        AgentStatus::WaitUser => "wait_user",
        AgentStatus::Completed => "completed",
        AgentStatus::Failed => "failed",
        AgentStatus::MaxCycles => "max_cycles",
    }
}

pub(crate) fn checkpoint_status_from_value(value: &str) -> Result<AgentStatus> {
    match value {
        "pending" => Ok(AgentStatus::Pending),
        "running" => Ok(AgentStatus::Running),
        "wait_user" => Ok(AgentStatus::WaitUser),
        "completed" => Ok(AgentStatus::Completed),
        "failed" => Ok(AgentStatus::Failed),
        "max_cycles" => Ok(AgentStatus::MaxCycles),
        other => Err(Error::new(
            ErrorKind::InvalidData,
            format!("unknown checkpoint status: {other}"),
        )),
    }
}

fn poisoned(name: &str) -> Error {
    Error::other(format!("{name} lock is poisoned"))
}
