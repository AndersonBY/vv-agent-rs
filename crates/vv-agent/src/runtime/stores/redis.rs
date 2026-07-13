use std::io::{Error, ErrorKind, Result};
use std::sync::Mutex;

use redis::{Commands, Connection};

use crate::runtime::checkpoint_codec;
use crate::runtime::state::{
    check_claim, claim_matches, clear_claim, validate_claim, validate_renew, Checkpoint,
    StateStore, StateStoreSpec,
};

const KEY_PREFIX: &str = "vv_agent:checkpoint:";

pub struct RedisStateStore {
    connection: Mutex<Connection>,
    redis_url: String,
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
        let redis_url = redis_url.as_ref().to_string();
        let client = redis::Client::open(redis_url.as_str()).map_err(redis_to_io)?;
        let connection = client.get_connection().map_err(redis_to_io)?;
        Ok(Self {
            connection: Mutex::new(connection),
            redis_url,
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

    fn transaction<T>(
        &self,
        key: &str,
        operation: impl FnMut(&mut Connection, &mut redis::Pipeline) -> redis::RedisResult<Option<T>>,
    ) -> Result<T> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| Error::other("redis state store lock is poisoned"))?;
        redis::transaction(&mut *connection, &[key], operation).map_err(redis_to_io)
    }
}

impl StateStore for RedisStateStore {
    fn create_checkpoint(&self, checkpoint: Checkpoint) -> Result<bool> {
        let key = Self::checkpoint_key(&checkpoint.task_id);
        let payload = Self::checkpoint_to_json(&checkpoint)?;
        self.connection
            .lock()
            .map_err(|_| Error::other("redis state store lock is poisoned"))?
            .set_nx(key, payload)
            .map_err(redis_to_io)
    }

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

    fn claim_checkpoint(
        &self,
        task_id: &str,
        cycle_index: u32,
        claim_token: &str,
        lease_expires_at_ms: u64,
        now_ms: u64,
    ) -> Result<Option<Checkpoint>> {
        validate_claim(cycle_index, claim_token, lease_expires_at_ms, now_ms)?;
        let key = Self::checkpoint_key(task_id);
        self.transaction(&key, |connection, pipe| {
            let Some(raw) = connection.get::<_, Option<String>>(&key)? else {
                return Ok(Some(None));
            };
            let mut checkpoint = Self::checkpoint_from_json(&raw).map_err(io_to_redis)?;
            check_claim(&checkpoint, cycle_index, now_ms).map_err(io_to_redis)?;
            checkpoint.revision = checkpoint.revision.checked_add(1).ok_or_else(|| {
                io_to_redis(Error::new(
                    ErrorKind::InvalidData,
                    "checkpoint revision overflow",
                ))
            })?;
            checkpoint.claim_token = Some(claim_token.to_string());
            checkpoint.claimed_cycle = Some(cycle_index);
            checkpoint.lease_expires_at_ms = Some(lease_expires_at_ms);
            let payload = Self::checkpoint_to_json(&checkpoint).map_err(io_to_redis)?;
            pipe.set(&key, payload).ignore();
            pipe.query(connection).map(|_: ()| Some(Some(checkpoint)))
        })
    }

    fn commit_checkpoint(
        &self,
        mut checkpoint: Checkpoint,
        claim_token: &str,
        expected_revision: u64,
    ) -> Result<bool> {
        let key = Self::checkpoint_key(&checkpoint.task_id);
        self.transaction(&key, |connection, pipe| {
            let current = connection.get::<_, Option<String>>(&key)?;
            let current = current
                .as_deref()
                .map(Self::checkpoint_from_json)
                .transpose()
                .map_err(io_to_redis)?;
            if !claim_matches(
                current.as_ref(),
                &checkpoint,
                claim_token,
                expected_revision,
            ) {
                return Ok(Some(false));
            }
            checkpoint.revision = expected_revision.checked_add(1).ok_or_else(|| {
                io_to_redis(Error::new(
                    ErrorKind::InvalidData,
                    "checkpoint revision overflow",
                ))
            })?;
            clear_claim(&mut checkpoint);
            let payload = Self::checkpoint_to_json(&checkpoint).map_err(io_to_redis)?;
            pipe.set(&key, payload).ignore();
            pipe.query(connection).map(|_: ()| Some(true))
        })
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
        let key = Self::checkpoint_key(task_id);
        self.transaction(&key, |connection, pipe| {
            let current = connection
                .get::<_, Option<String>>(&key)?
                .as_deref()
                .map(Self::checkpoint_from_json)
                .transpose()
                .map_err(io_to_redis)?;
            let Some(mut checkpoint) = current else {
                return Ok(Some(false));
            };
            if checkpoint.revision != expected_revision
                || checkpoint.claim_token.as_deref() != Some(claim_token)
                || checkpoint.lease_expires_at_ms.unwrap_or(0) <= now_ms
            {
                return Ok(Some(false));
            }
            checkpoint.lease_expires_at_ms = Some(lease_expires_at_ms);
            let payload = Self::checkpoint_to_json(&checkpoint).map_err(io_to_redis)?;
            pipe.set(&key, payload).ignore();
            pipe.query(connection).map(|_: ()| Some(true))
        })
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
        let key = Self::checkpoint_key(&checkpoint.task_id);
        self.transaction(&key, |connection, pipe| {
            let current = connection.get::<_, Option<String>>(&key)?;
            let current = current
                .as_deref()
                .map(Self::checkpoint_from_json)
                .transpose()
                .map_err(io_to_redis)?;
            if current.as_ref().is_none_or(|current| {
                current.revision != expected_revision
                    || current.claim_token.is_some()
                    || current.terminal_result.is_some()
            }) {
                return Ok(Some(false));
            }
            checkpoint.revision = expected_revision.checked_add(1).ok_or_else(|| {
                io_to_redis(Error::new(
                    ErrorKind::InvalidData,
                    "checkpoint revision overflow",
                ))
            })?;
            clear_claim(&mut checkpoint);
            let payload = Self::checkpoint_to_json(&checkpoint).map_err(io_to_redis)?;
            pipe.set(&key, payload).ignore();
            pipe.query(connection).map(|_: ()| Some(true))
        })
    }

    fn delete_checkpoint(&self, task_id: &str) -> Result<()> {
        let key = Self::checkpoint_key(task_id);
        self.connection
            .lock()
            .map_err(|_| Error::other("redis state store lock is poisoned"))?
            .del::<_, ()>(key)
            .map_err(redis_to_io)
    }

    fn acknowledge_terminal(&self, task_id: &str, expected_revision: u64) -> Result<bool> {
        let key = Self::checkpoint_key(task_id);
        self.transaction(&key, |connection, pipe| {
            let current = connection.get::<_, Option<String>>(&key)?;
            let matches = current
                .as_deref()
                .map(Self::checkpoint_from_json)
                .transpose()
                .map_err(io_to_redis)?
                .is_some_and(|checkpoint| {
                    checkpoint.revision == expected_revision && checkpoint.terminal_result.is_some()
                });
            if !matches {
                return Ok(Some(false));
            }
            pipe.del(&key).ignore();
            pipe.query(connection).map(|_: ()| Some(true))
        })
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

    fn state_store_spec(&self) -> Option<StateStoreSpec> {
        StateStoreSpec::redis(&self.redis_url).ok()
    }
}

fn redis_to_io(error: redis::RedisError) -> Error {
    Error::other(error.to_string())
}

fn io_to_redis(error: Error) -> redis::RedisError {
    redis::RedisError::from((
        redis::ErrorKind::TypeError,
        "checkpoint state error",
        error.to_string(),
    ))
}
