use std::collections::BTreeMap;
use std::io::{Error, ErrorKind, Result};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::{AgentStatus, CycleRecord, Message, Metadata};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub task_id: String,
    pub cycle_index: u32,
    pub status: AgentStatus,
    pub messages: Vec<Message>,
    pub cycles: Vec<CycleRecord>,
    pub shared_state: Metadata,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub claim_token: Option<String>,
    #[serde(default)]
    pub claimed_cycle: Option<u32>,
    #[serde(default)]
    pub lease_expires_at_ms: Option<u64>,
    #[serde(default)]
    pub terminal_result: Option<crate::types::AgentResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateStoreKind {
    Sqlite,
    Redis,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateStoreSpec {
    pub kind: StateStoreKind,
    pub location: String,
}

impl StateStoreSpec {
    pub fn sqlite(location: impl Into<String>) -> Result<Self> {
        Self::new(StateStoreKind::Sqlite, location)
    }

    pub fn redis(location: impl Into<String>) -> Result<Self> {
        Self::new(StateStoreKind::Redis, location)
    }

    fn new(kind: StateStoreKind, location: impl Into<String>) -> Result<Self> {
        let location = location.into();
        if location.trim().is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "state store location must be a non-empty string",
            ));
        }
        Ok(Self { kind, location })
    }

    pub fn to_dict(&self) -> Value {
        serde_json::json!({
            "kind": match self.kind {
                StateStoreKind::Sqlite => "sqlite",
                StateStoreKind::Redis => "redis",
            },
            "location": self.location,
        })
    }

    pub fn from_dict(payload: &Value) -> Result<Self> {
        let object = payload
            .as_object()
            .ok_or_else(|| Error::new(ErrorKind::InvalidData, "state_store must be an object"))?;
        let kind = match object.get("kind").and_then(Value::as_str) {
            Some("sqlite") => StateStoreKind::Sqlite,
            Some("redis") => StateStoreKind::Redis,
            _ => {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "state_store.kind must be 'sqlite' or 'redis'",
                ))
            }
        };
        let location = object
            .get("location")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidData,
                    "state_store.location must be a non-empty string",
                )
            })?;
        Self::new(kind, location)
    }

    pub fn build(&self) -> Result<Arc<dyn StateStore>> {
        match self.kind {
            StateStoreKind::Sqlite => Ok(Arc::new(
                crate::runtime::stores::sqlite::SqliteStateStore::new(&self.location)?,
            )),
            StateStoreKind::Redis => Ok(Arc::new(
                crate::runtime::stores::redis::RedisStateStore::new(&self.location)?,
            )),
        }
    }
}

pub trait StateStore: Send + Sync {
    fn create_checkpoint(&self, checkpoint: Checkpoint) -> Result<bool>;
    fn save_checkpoint(&self, checkpoint: Checkpoint) -> Result<()>;
    fn load_checkpoint(&self, task_id: &str) -> Result<Option<Checkpoint>>;
    fn claim_checkpoint(
        &self,
        task_id: &str,
        cycle_index: u32,
        claim_token: &str,
        lease_expires_at_ms: u64,
        now_ms: u64,
    ) -> Result<Option<Checkpoint>>;
    fn commit_checkpoint(
        &self,
        checkpoint: Checkpoint,
        claim_token: &str,
        expected_revision: u64,
    ) -> Result<bool>;
    fn renew_checkpoint_claim(
        &self,
        task_id: &str,
        claim_token: &str,
        expected_revision: u64,
        lease_expires_at_ms: u64,
        now_ms: u64,
    ) -> Result<bool>;
    fn finalize_checkpoint(&self, checkpoint: Checkpoint, expected_revision: u64) -> Result<bool>;
    fn delete_checkpoint(&self, task_id: &str) -> Result<()>;
    fn acknowledge_terminal(&self, task_id: &str, expected_revision: u64) -> Result<bool>;
    fn list_checkpoints(&self) -> Result<Vec<String>>;
    fn state_store_spec(&self) -> Option<StateStoreSpec>;
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
    fn create_checkpoint(&self, checkpoint: Checkpoint) -> Result<bool> {
        crate::runtime::checkpoint_codec::validate_checkpoint(&checkpoint)?;
        let mut checkpoints = self
            .checkpoints
            .lock()
            .map_err(|_| poisoned("state store"))?;
        if checkpoints.contains_key(&checkpoint.task_id) {
            return Ok(false);
        }
        checkpoints.insert(checkpoint.task_id.clone(), checkpoint);
        Ok(true)
    }

    fn save_checkpoint(&self, checkpoint: Checkpoint) -> Result<()> {
        crate::runtime::checkpoint_codec::validate_checkpoint(&checkpoint)?;
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

    fn claim_checkpoint(
        &self,
        task_id: &str,
        cycle_index: u32,
        claim_token: &str,
        lease_expires_at_ms: u64,
        now_ms: u64,
    ) -> Result<Option<Checkpoint>> {
        validate_claim(cycle_index, claim_token, lease_expires_at_ms, now_ms)?;
        let mut checkpoints = self
            .checkpoints
            .lock()
            .map_err(|_| poisoned("state store"))?;
        let Some(checkpoint) = checkpoints.get_mut(task_id) else {
            return Ok(None);
        };
        check_claim(checkpoint, cycle_index, now_ms)?;
        checkpoint.revision = checkpoint.revision.saturating_add(1);
        checkpoint.claim_token = Some(claim_token.to_string());
        checkpoint.claimed_cycle = Some(cycle_index);
        checkpoint.lease_expires_at_ms = Some(lease_expires_at_ms);
        Ok(Some(checkpoint.clone()))
    }

    fn commit_checkpoint(
        &self,
        mut checkpoint: Checkpoint,
        claim_token: &str,
        expected_revision: u64,
    ) -> Result<bool> {
        let mut checkpoints = self
            .checkpoints
            .lock()
            .map_err(|_| poisoned("state store"))?;
        let current = checkpoints.get(&checkpoint.task_id);
        if !claim_matches(current, &checkpoint, claim_token, expected_revision) {
            return Ok(false);
        }
        checkpoint.revision = expected_revision.saturating_add(1);
        clear_claim(&mut checkpoint);
        crate::runtime::checkpoint_codec::validate_checkpoint(&checkpoint)?;
        checkpoints.insert(checkpoint.task_id.clone(), checkpoint);
        Ok(true)
    }

    fn renew_checkpoint_claim(
        &self,
        task_id: &str,
        claim_token: &str,
        expected_revision: u64,
        lease_expires_at_ms: u64,
        now_ms: u64,
    ) -> Result<bool> {
        validate_renew(claim_token, expected_revision, lease_expires_at_ms, now_ms)?;
        let mut checkpoints = self
            .checkpoints
            .lock()
            .map_err(|_| poisoned("state store"))?;
        let Some(checkpoint) = checkpoints.get_mut(task_id) else {
            return Ok(false);
        };
        if checkpoint.revision != expected_revision
            || checkpoint.claim_token.as_deref() != Some(claim_token)
            || checkpoint.lease_expires_at_ms.unwrap_or(0) <= now_ms
        {
            return Ok(false);
        }
        checkpoint.lease_expires_at_ms = Some(lease_expires_at_ms);
        Ok(true)
    }

    fn finalize_checkpoint(
        &self,
        mut checkpoint: Checkpoint,
        expected_revision: u64,
    ) -> Result<bool> {
        if checkpoint.terminal_result.is_none() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "finalized checkpoint must include terminal_result",
            ));
        }
        crate::runtime::checkpoint_codec::validate_checkpoint(&checkpoint)?;
        let mut checkpoints = self
            .checkpoints
            .lock()
            .map_err(|_| poisoned("state store"))?;
        let Some(current) = checkpoints.get(&checkpoint.task_id) else {
            return Ok(false);
        };
        if current.revision != expected_revision
            || current.claim_token.is_some()
            || current.terminal_result.is_some()
        {
            return Ok(false);
        }
        checkpoint.revision = expected_revision.saturating_add(1);
        clear_claim(&mut checkpoint);
        checkpoints.insert(checkpoint.task_id.clone(), checkpoint);
        Ok(true)
    }

    fn delete_checkpoint(&self, task_id: &str) -> Result<()> {
        self.checkpoints
            .lock()
            .map_err(|_| poisoned("state store"))?
            .remove(task_id);
        Ok(())
    }

    fn acknowledge_terminal(&self, task_id: &str, expected_revision: u64) -> Result<bool> {
        let mut checkpoints = self
            .checkpoints
            .lock()
            .map_err(|_| poisoned("state store"))?;
        if checkpoints.get(task_id).is_none_or(|checkpoint| {
            checkpoint.revision != expected_revision || checkpoint.terminal_result.is_none()
        }) {
            return Ok(false);
        }
        checkpoints.remove(task_id);
        Ok(true)
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

    fn state_store_spec(&self) -> Option<StateStoreSpec> {
        None
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

pub(crate) fn validate_claim(
    cycle_index: u32,
    claim_token: &str,
    lease_expires_at_ms: u64,
    now_ms: u64,
) -> Result<()> {
    if cycle_index == 0 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "claimed cycle_index must be between 1 and 4294967295",
        ));
    }
    if claim_token.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "claim_token must be a non-empty string",
        ));
    }
    if lease_expires_at_ms <= now_ms {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "lease_expires_at_ms must be greater than now_ms",
        ));
    }
    Ok(())
}

pub(crate) fn validate_renew(
    claim_token: &str,
    _expected_revision: u64,
    lease_expires_at_ms: u64,
    now_ms: u64,
) -> Result<()> {
    if claim_token.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "claim_token must be a non-empty string",
        ));
    }
    if lease_expires_at_ms <= now_ms {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "lease_expires_at_ms must be greater than now_ms",
        ));
    }
    Ok(())
}

pub(crate) fn check_claim(checkpoint: &Checkpoint, cycle_index: u32, now_ms: u64) -> Result<()> {
    let expected_cycle = cycle_index - 1;
    if checkpoint.terminal_result.is_some() || checkpoint.status != AgentStatus::Running {
        return Err(Error::new(
            ErrorKind::AlreadyExists,
            format!("checkpoint for task {} is terminal", checkpoint.task_id),
        ));
    }
    if checkpoint.cycle_index != expected_cycle {
        return Err(Error::new(
            ErrorKind::AlreadyExists,
            format!(
                "checkpoint cycle conflict for task {}: expected {}, found {}",
                checkpoint.task_id, expected_cycle, checkpoint.cycle_index
            ),
        ));
    }
    if checkpoint.claim_token.is_some() && checkpoint.lease_expires_at_ms.unwrap_or(0) > now_ms {
        return Err(Error::new(
            ErrorKind::AlreadyExists,
            format!(
                "checkpoint cycle {cycle_index} for task {} is already claimed",
                checkpoint.task_id
            ),
        ));
    }
    Ok(())
}

pub(crate) fn claim_matches(
    current: Option<&Checkpoint>,
    checkpoint: &Checkpoint,
    claim_token: &str,
    expected_revision: u64,
) -> bool {
    current.is_some_and(|current| {
        current.revision == expected_revision
            && current.claim_token.as_deref() == Some(claim_token)
            && current.claimed_cycle.is_some()
            && checkpoint.cycle_index == current.claimed_cycle.unwrap_or_default()
    })
}

pub(crate) fn clear_claim(checkpoint: &mut Checkpoint) {
    checkpoint.claim_token = None;
    checkpoint.claimed_cycle = None;
    checkpoint.lease_expires_at_ms = None;
}
