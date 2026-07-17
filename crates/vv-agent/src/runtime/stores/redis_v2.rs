//! Redis checkpoint v2 store.

use std::sync::Mutex;
use std::time::Duration;

use redis::{Commands, Connection, Pipeline};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::checkpoint::{CheckpointError, CheckpointResult, ClaimMode, EventCursor};
use crate::runtime::checkpoint_codec_v2::{checkpoint_v2_from_json, checkpoint_v2_to_json};
use crate::runtime::state_v2::{
    apply_claim, claim_candidate, prepare_ack, prepare_commit, prepare_event_delivery,
    prepare_finalize, prepare_finalize_claimed, prepare_progress, prepare_suspend,
    CheckpointStoreV2, CheckpointV2,
};

const KEY_PREFIX: &str = "vv-agent:checkpoint:v2:";
const LEASE_SUFFIX: &str = ":lease";
const IO_TIMEOUT: Duration = Duration::from_secs(1);
const TRANSACTION_MAX_ATTEMPTS: usize = 8;
const MAX_EXTENSION_STATE_BYTES: u64 = crate::checkpoint::MAX_WIRE_INTEGER;

pub struct RedisCheckpointStoreV2 {
    connection: Mutex<Connection>,
    redis_url: String,
}

impl std::fmt::Debug for RedisCheckpointStoreV2 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RedisCheckpointStoreV2")
            .field("redis_url", &self.redis_url)
            .finish_non_exhaustive()
    }
}

impl RedisCheckpointStoreV2 {
    pub fn new(redis_url: impl AsRef<str>) -> CheckpointResult<Self> {
        let redis_url = redis_url.as_ref().to_string();
        let client = redis::Client::open(redis_url.as_str()).map_err(redis_error)?;
        let connection = client
            .get_connection_with_timeout(IO_TIMEOUT)
            .map_err(redis_error)?;
        connection
            .set_read_timeout(Some(IO_TIMEOUT))
            .map_err(redis_error)?;
        connection
            .set_write_timeout(Some(IO_TIMEOUT))
            .map_err(redis_error)?;
        Ok(Self {
            connection: Mutex::new(connection),
            redis_url,
        })
    }

    pub fn data_key(checkpoint_key: &str) -> String {
        let digest = Sha256::digest(checkpoint_key.as_bytes());
        format!("{KEY_PREFIX}{digest:x}")
    }

    pub fn lease_key(checkpoint_key: &str) -> String {
        format!("{}{LEASE_SUFFIX}", Self::data_key(checkpoint_key))
    }

    pub fn checkpoint_v2_key(checkpoint_key: &str) -> String {
        Self::data_key(checkpoint_key)
    }

    pub fn checkpoint_v2_lease_key(checkpoint_key: &str) -> String {
        Self::lease_key(checkpoint_key)
    }

    pub fn redis_url(&self) -> &str {
        &self.redis_url
    }

    fn lock(&self) -> CheckpointResult<std::sync::MutexGuard<'_, Connection>> {
        self.connection.lock().map_err(|_| {
            CheckpointError::new(
                "checkpoint_store_lock_poisoned",
                "Redis store lock poisoned",
            )
        })
    }

    fn load_from_connection(
        connection: &mut Connection,
        data_key: &str,
        lease_key: &str,
    ) -> CheckpointResult<Option<CheckpointV2>> {
        for _ in 0..TRANSACTION_MAX_ATTEMPTS {
            let Some(raw) = connection
                .get::<_, Option<String>>(data_key)
                .map_err(redis_error)?
            else {
                return Ok(None);
            };
            let lease = connection
                .get::<_, Option<u64>>(lease_key)
                .map_err(redis_error)?;
            let raw_again = connection
                .get::<_, Option<String>>(data_key)
                .map_err(redis_error)?;
            if raw_again.as_deref() != Some(raw.as_str()) {
                continue;
            }
            return decode_storage(&raw, lease).map(Some);
        }
        Err(CheckpointError::new(
            "checkpoint_store_read_conflict",
            "Redis checkpoint load could not obtain a stable snapshot",
        ))
    }

    fn transaction<T>(
        &self,
        data_key: &str,
        lease_key: &str,
        operation: impl Fn(&mut Connection, &mut Pipeline) -> CheckpointResult<Option<T>>,
    ) -> CheckpointResult<T> {
        let mut connection = self.lock()?;
        for _ in 0..TRANSACTION_MAX_ATTEMPTS {
            redis::cmd("WATCH")
                .arg(data_key)
                .arg(lease_key)
                .query::<()>(&mut *connection)
                .map_err(redis_error)?;
            let mut pipeline = redis::pipe();
            pipeline.atomic();
            match operation(&mut connection, &mut pipeline)? {
                None => {
                    redis::cmd("UNWATCH")
                        .query::<()>(&mut *connection)
                        .map_err(redis_error)?;
                    return Err(CheckpointError::new(
                        "checkpoint_store_conflict",
                        "checkpoint operation did not match its compare-and-set precondition",
                    ));
                }
                Some(value) => match pipeline.query::<Option<()>>(&mut *connection) {
                    Ok(Some(())) => {
                        redis::cmd("UNWATCH")
                            .query::<()>(&mut *connection)
                            .map_err(redis_error)?;
                        return Ok(value);
                    }
                    Ok(None) => continue,
                    Err(error) => return Err(redis_error(error)),
                },
            }
        }
        Err(CheckpointError::new(
            "checkpoint_store_transaction_retry_exhausted",
            "Redis checkpoint transaction retry limit exceeded",
        ))
    }
}

impl CheckpointStoreV2 for RedisCheckpointStoreV2 {
    fn create_checkpoint_v2(&self, checkpoint: CheckpointV2) -> CheckpointResult<bool> {
        checkpoint.validate()?;
        let data_key = Self::data_key(&checkpoint.checkpoint_key);
        let lease_key = Self::lease_key(&checkpoint.checkpoint_key);
        let payload = checkpoint_v2_to_json(&checkpoint, MAX_EXTENSION_STATE_BYTES)?;
        let mut connection = self.lock()?;
        let created: bool = connection.set_nx(&data_key, payload).map_err(redis_error)?;
        if created {
            connection.del::<_, ()>(&lease_key).map_err(redis_error)?;
        }
        Ok(created)
    }

    fn load_checkpoint_v2(&self, checkpoint_key: &str) -> CheckpointResult<Option<CheckpointV2>> {
        let data_key = Self::data_key(checkpoint_key);
        let lease_key = Self::lease_key(checkpoint_key);
        let mut connection = self.lock()?;
        Self::load_from_connection(&mut connection, &data_key, &lease_key)
    }

    fn claim_checkpoint_v2(
        &self,
        checkpoint_key: &str,
        cycle_index: u64,
        claim_token: &str,
        lease_expires_at_ms: u64,
        now_ms: u64,
        claim_mode: ClaimMode,
    ) -> CheckpointResult<Option<CheckpointV2>> {
        if claim_token.trim().is_empty() || lease_expires_at_ms <= now_ms {
            return Err(CheckpointError::new(
                "checkpoint_claim_invalid",
                "claim token must be non-empty and lease must be in the future",
            ));
        }
        let data_key = Self::data_key(checkpoint_key);
        let lease_key = Self::lease_key(checkpoint_key);
        let result = self.transaction(&data_key, &lease_key, |connection, pipeline| {
            let Some(raw) = connection
                .get::<_, Option<String>>(&data_key)
                .map_err(redis_error)?
            else {
                return Ok(None);
            };
            let lease = connection
                .get::<_, Option<u64>>(&lease_key)
                .map_err(redis_error)?;
            let current = decode_storage(&raw, lease)?;
            if !claim_candidate(&current, cycle_index, now_ms, claim_mode)? {
                return Ok(None);
            }
            let mut claimed = current;
            apply_claim(
                &mut claimed,
                cycle_index,
                claim_token,
                lease_expires_at_ms,
                claim_mode,
            )?;
            let payload = checkpoint_v2_to_json(&claimed, MAX_EXTENSION_STATE_BYTES)?;
            pipeline.set(&data_key, payload).ignore();
            pipeline.set(&lease_key, lease_expires_at_ms).ignore();
            Ok(Some(claimed))
        });
        match result {
            Ok(value) => Ok(Some(value)),
            Err(error) if error.code() == "checkpoint_store_conflict" => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn progress_checkpoint_v2(
        &self,
        checkpoint: CheckpointV2,
        claim_token: &str,
        expected_revision: u64,
    ) -> CheckpointResult<bool> {
        self.replace_claimed(
            checkpoint,
            claim_token,
            expected_revision,
            ReplaceKind::Progress,
        )
    }

    fn suspend_checkpoint_v2(
        &self,
        checkpoint: CheckpointV2,
        claim_token: &str,
        expected_revision: u64,
    ) -> CheckpointResult<bool> {
        self.replace_claimed(
            checkpoint,
            claim_token,
            expected_revision,
            ReplaceKind::Suspend,
        )
    }

    fn commit_checkpoint_v2(
        &self,
        checkpoint: CheckpointV2,
        claim_token: &str,
        expected_revision: u64,
    ) -> CheckpointResult<bool> {
        self.replace_claimed(
            checkpoint,
            claim_token,
            expected_revision,
            ReplaceKind::Commit,
        )
    }

    fn finalize_claimed_v2(
        &self,
        checkpoint: CheckpointV2,
        claim_token: &str,
        expected_revision: u64,
    ) -> CheckpointResult<bool> {
        self.replace_claimed(
            checkpoint,
            claim_token,
            expected_revision,
            ReplaceKind::FinalizeClaimed,
        )
    }

    fn finalize_checkpoint_v2(
        &self,
        checkpoint: CheckpointV2,
        expected_revision: u64,
    ) -> CheckpointResult<bool> {
        let data_key = Self::data_key(&checkpoint.checkpoint_key);
        let lease_key = Self::lease_key(&checkpoint.checkpoint_key);
        let result = self.transaction(&data_key, &lease_key, |connection, pipeline| {
            let Some(raw) = connection
                .get::<_, Option<String>>(&data_key)
                .map_err(redis_error)?
            else {
                return Ok(None);
            };
            let current = decode_storage(
                &raw,
                connection
                    .get::<_, Option<u64>>(&lease_key)
                    .map_err(redis_error)?,
            )?;
            let Some(updated) = prepare_finalize(&current, checkpoint.clone(), expected_revision)?
            else {
                return Ok(None);
            };
            let payload = checkpoint_v2_to_json(&updated, MAX_EXTENSION_STATE_BYTES)?;
            pipeline.set(&data_key, payload).ignore();
            pipeline.del(&lease_key).ignore();
            Ok(Some(true))
        });
        match result {
            Ok(value) => Ok(value),
            Err(error) if error.code() == "checkpoint_store_conflict" => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn renew_checkpoint_claim_v2(
        &self,
        checkpoint_key: &str,
        claim_token: &str,
        lease_expires_at_ms: u64,
        now_ms: u64,
    ) -> CheckpointResult<bool> {
        if claim_token.trim().is_empty() || lease_expires_at_ms <= now_ms {
            return Err(CheckpointError::new(
                "checkpoint_claim_invalid",
                "claim token must be non-empty and lease must be in the future",
            ));
        }
        let data_key = Self::data_key(checkpoint_key);
        let lease_key = Self::lease_key(checkpoint_key);
        let result = self.transaction(&data_key, &lease_key, |connection, pipeline| {
            let Some(raw) = connection
                .get::<_, Option<String>>(&data_key)
                .map_err(redis_error)?
            else {
                return Ok(None);
            };
            let current_lease = connection
                .get::<_, Option<u64>>(&lease_key)
                .map_err(redis_error)?;
            let current = decode_storage(&raw, current_lease)?;
            if current.claim_token.as_deref() != Some(claim_token)
                || current
                    .lease_expires_at_ms
                    .is_none_or(|expiry| expiry <= now_ms)
            {
                return Ok(None);
            }
            pipeline.set(&lease_key, lease_expires_at_ms).ignore();
            Ok(Some(true))
        });
        match result {
            Ok(value) => Ok(value),
            Err(error) if error.code() == "checkpoint_store_conflict" => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn acknowledge_terminal_v2(
        &self,
        checkpoint_key: &str,
        expected_revision: u64,
    ) -> CheckpointResult<bool> {
        let data_key = Self::data_key(checkpoint_key);
        let lease_key = Self::lease_key(checkpoint_key);
        let result = self.transaction(&data_key, &lease_key, |connection, pipeline| {
            let Some(raw) = connection
                .get::<_, Option<String>>(&data_key)
                .map_err(redis_error)?
            else {
                return Ok(None);
            };
            let current = decode_storage(
                &raw,
                connection
                    .get::<_, Option<u64>>(&lease_key)
                    .map_err(redis_error)?,
            )?;
            let Some(updated) = prepare_ack(&current, expected_revision)? else {
                return Ok(None);
            };
            let payload = checkpoint_v2_to_json(&updated, MAX_EXTENSION_STATE_BYTES)?;
            pipeline.set(&data_key, payload).ignore();
            pipeline.del(&lease_key).ignore();
            Ok(Some(true))
        });
        match result {
            Ok(value) => Ok(value),
            Err(error) if error.code() == "checkpoint_store_conflict" => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn record_event_delivery_v2(
        &self,
        checkpoint_key: &str,
        claim_token: Option<&str>,
        expected_revision: u64,
        event_id: &str,
        payload_digest: &str,
        cursor: EventCursor,
    ) -> CheckpointResult<bool> {
        let data_key = Self::data_key(checkpoint_key);
        let lease_key = Self::lease_key(checkpoint_key);
        let result = self.transaction(&data_key, &lease_key, |connection, pipeline| {
            let Some(raw) = connection
                .get::<_, Option<String>>(&data_key)
                .map_err(redis_error)?
            else {
                return Ok(None);
            };
            let current = decode_storage(
                &raw,
                connection
                    .get::<_, Option<u64>>(&lease_key)
                    .map_err(redis_error)?,
            )?;
            let Some(updated) = prepare_event_delivery(
                &current,
                claim_token,
                expected_revision,
                event_id,
                payload_digest,
                cursor.clone(),
            )?
            else {
                return Ok(None);
            };
            let payload = checkpoint_v2_to_json(&updated, MAX_EXTENSION_STATE_BYTES)?;
            pipeline.set(&data_key, payload).ignore();
            if updated.claim_token.is_none() {
                pipeline.del(&lease_key).ignore();
            }
            Ok(Some(true))
        });
        match result {
            Ok(value) => Ok(value),
            Err(error) if error.code() == "checkpoint_store_conflict" => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn delete_checkpoint_v2(&self, checkpoint_key: &str) -> CheckpointResult<()> {
        let mut connection = self.lock()?;
        let data_key = Self::data_key(checkpoint_key);
        let lease_key = Self::lease_key(checkpoint_key);
        let keys = [data_key.as_str(), lease_key.as_str()];
        let _: usize = connection.del(&keys).map_err(redis_error)?;
        Ok(())
    }

    fn list_checkpoints_v2(&self) -> CheckpointResult<Vec<String>> {
        let mut connection = self.lock()?;
        let keys = connection
            .scan_match::<_, String>(format!("{KEY_PREFIX}*"))
            .map_err(redis_error)?
            .filter(|key| !key.ends_with(LEASE_SUFFIX))
            .collect::<Vec<_>>();
        let mut checkpoint_keys = Vec::new();
        for key in keys {
            let Some(raw) = connection
                .get::<_, Option<String>>(&key)
                .map_err(redis_error)?
            else {
                continue;
            };
            let checkpoint = decode_storage(
                &raw,
                connection
                    .get::<_, Option<u64>>(format!("{key}{LEASE_SUFFIX}"))
                    .map_err(redis_error)?,
            )?;
            checkpoint_keys.push(checkpoint.checkpoint_key);
        }
        checkpoint_keys.sort();
        Ok(checkpoint_keys)
    }
}

impl RedisCheckpointStoreV2 {
    fn replace_claimed(
        &self,
        checkpoint: CheckpointV2,
        claim_token: &str,
        expected_revision: u64,
        kind: ReplaceKind,
    ) -> CheckpointResult<bool> {
        let data_key = Self::data_key(&checkpoint.checkpoint_key);
        let lease_key = Self::lease_key(&checkpoint.checkpoint_key);
        let result = self.transaction(&data_key, &lease_key, |connection, pipeline| {
            let Some(raw) = connection
                .get::<_, Option<String>>(&data_key)
                .map_err(redis_error)?
            else {
                return Ok(None);
            };
            let current = decode_storage(
                &raw,
                connection
                    .get::<_, Option<u64>>(&lease_key)
                    .map_err(redis_error)?,
            )?;
            let updated = match kind {
                ReplaceKind::Progress => {
                    prepare_progress(&current, checkpoint.clone(), claim_token, expected_revision)?
                }
                ReplaceKind::Suspend => {
                    prepare_suspend(&current, checkpoint.clone(), claim_token, expected_revision)?
                }
                ReplaceKind::Commit => {
                    prepare_commit(&current, checkpoint.clone(), claim_token, expected_revision)?
                }
                ReplaceKind::FinalizeClaimed => prepare_finalize_claimed(
                    &current,
                    checkpoint.clone(),
                    claim_token,
                    expected_revision,
                )?,
            };
            let Some(updated) = updated else {
                return Ok(None);
            };
            let payload = checkpoint_v2_to_json(&updated, MAX_EXTENSION_STATE_BYTES)?;
            pipeline.set(&data_key, payload).ignore();
            if updated.claim_token.is_none() {
                pipeline.del(&lease_key).ignore();
            }
            Ok(Some(true))
        });
        match result {
            Ok(value) => Ok(value),
            Err(error) if error.code() == "checkpoint_store_conflict" => Ok(false),
            Err(error) => Err(error),
        }
    }
}

#[derive(Clone, Copy)]
enum ReplaceKind {
    Progress,
    Suspend,
    Commit,
    FinalizeClaimed,
}

fn decode_storage(raw: &str, lease: Option<u64>) -> CheckpointResult<CheckpointV2> {
    let mut value: Value = serde_json::from_str(raw)
        .map_err(|error| CheckpointError::new("checkpoint_json_invalid", error.to_string()))?;
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "lease_expires_at_ms".to_string(),
            lease.map_or(Value::Null, Value::from),
        );
    }
    let payload = serde_json::to_string(&value)
        .map_err(|error| CheckpointError::new("checkpoint_json_invalid", error.to_string()))?;
    checkpoint_v2_from_json(&payload, MAX_EXTENSION_STATE_BYTES)
}

fn redis_error(error: redis::RedisError) -> CheckpointError {
    CheckpointError::new("checkpoint_store_redis", error.to_string())
}

pub type RedisStateStoreV2 = RedisCheckpointStoreV2;
