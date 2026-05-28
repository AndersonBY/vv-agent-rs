use std::io::{Error, Result};
use std::sync::Mutex;

use redis::{Commands, Connection};

use crate::runtime::checkpoint_codec;
use crate::runtime::state::{Checkpoint, StateStore};

const KEY_PREFIX: &str = "vv_agent:checkpoint:";

pub struct RedisStateStore {
    connection: Mutex<Connection>,
}

impl std::fmt::Debug for RedisStateStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RedisStateStore")
            .finish_non_exhaustive()
    }
}

impl RedisStateStore {
    pub fn new(redis_url: impl AsRef<str>) -> Result<Self> {
        let client = redis::Client::open(redis_url.as_ref()).map_err(redis_to_io)?;
        let connection = client.get_connection().map_err(redis_to_io)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn checkpoint_key(task_id: &str) -> String {
        format!("{KEY_PREFIX}{task_id}")
    }

    pub fn checkpoint_to_json(checkpoint: &Checkpoint) -> Result<String> {
        checkpoint_codec::checkpoint_to_json(checkpoint)
    }

    pub fn checkpoint_from_json(raw: &str) -> Result<Checkpoint> {
        checkpoint_codec::checkpoint_from_json(raw)
    }
}

impl StateStore for RedisStateStore {
    fn save_checkpoint(&self, checkpoint: Checkpoint) -> Result<()> {
        let key = Self::checkpoint_key(&checkpoint.task_id);
        let payload = Self::checkpoint_to_json(&checkpoint)?;
        self.connection
            .lock()
            .map_err(|_| Error::other("redis state store lock is poisoned"))?
            .set::<_, _, ()>(key, payload)
            .map_err(redis_to_io)
    }

    fn load_checkpoint(&self, task_id: &str) -> Result<Option<Checkpoint>> {
        let key = Self::checkpoint_key(task_id);
        let raw = self
            .connection
            .lock()
            .map_err(|_| Error::other("redis state store lock is poisoned"))?
            .get::<_, Option<String>>(key)
            .map_err(redis_to_io)?;
        raw.as_deref().map(Self::checkpoint_from_json).transpose()
    }

    fn delete_checkpoint(&self, task_id: &str) -> Result<()> {
        let key = Self::checkpoint_key(task_id);
        self.connection
            .lock()
            .map_err(|_| Error::other("redis state store lock is poisoned"))?
            .del::<_, ()>(key)
            .map_err(redis_to_io)
    }

    fn list_checkpoints(&self) -> Result<Vec<String>> {
        let pattern = format!("{KEY_PREFIX}*");
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| Error::other("redis state store lock is poisoned"))?;
        let iter = connection
            .scan_match::<_, String>(pattern)
            .map_err(redis_to_io)?;
        let mut keys = iter
            .map(|key| key.strip_prefix(KEY_PREFIX).unwrap_or(&key).to_string())
            .collect::<Vec<_>>();
        keys.sort();
        Ok(keys)
    }
}

fn redis_to_io(error: redis::RedisError) -> Error {
    Error::other(error.to_string())
}
