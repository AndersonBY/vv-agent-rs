//! Redis checkpoint store.

use std::sync::Mutex;
use std::time::Duration;

use redis::{Commands, Connection, Pipeline};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::checkpoint::{
    notification_id_for, record_id_for, CheckpointError, CheckpointResult, ClaimMode,
    ControllerCommand, ControllerCommandReceipt, ControllerCommandResolution, EventCursor,
    HostInteractionNotificationPayload, HostInteractionNotificationRecord, HostInteractionOutcome,
    HostInteractionRecord, HostInteractionRecoveryEnvelope, HostInteractionRecoveryResult,
    HostInteractionRequest, NotificationOutboxState, HOST_INTERACTION_NOTIFICATION_SCHEMA,
    HOST_INTERACTION_RECORD_SCHEMA,
};
use crate::events::{EventId, RunEvent, RunEventPayload};
use crate::runtime::checkpoint_codec::{checkpoint_from_json, checkpoint_to_json};
use crate::runtime::state::{
    apply_claim, claim_candidate, prepare_ack, prepare_commit, prepare_event_delivery,
    prepare_finalize, prepare_finalize_claimed, prepare_progress, prepare_suspend, Checkpoint,
    CheckpointStore,
};

const KEY_PREFIX: &str = "vv-agent:checkpoint:";
const LEASE_SUFFIX: &str = ":lease";
const DEFERRED_RECEIPT_PREFIX: &str = "vv-agent:deferred-receipt:";
const DEFERRED_RECEIPTS_BY_CHECKPOINT_PREFIX: &str = "vv-agent:deferred-receipts-by-checkpoint:";
const HOST_INTERACTION_PREFIX: &str = "vv-agent:host-interaction:";
const HOST_INTERACTION_NOTIFICATION_PREFIX: &str = "vv-agent:host-interaction-notification:";
const HOST_INTERACTIONS_BY_CHECKPOINT_PREFIX: &str = "vv-agent:host-interactions-by-checkpoint:";
const HOST_INTERACTION_NOTIFICATIONS_BY_CHECKPOINT_PREFIX: &str =
    "vv-agent:host-interaction-notifications-by-checkpoint:";
const CONTROLLER_COMMAND_PREFIX: &str = "vv-agent:controller-command:";
const CONTROLLER_RECEIPTS_BY_CHECKPOINT_PREFIX: &str =
    "vv-agent:controller-commands-by-checkpoint:";
const CHECKPOINT_KEYS_INDEX: &str = "vv-agent:checkpoint-keys";
const IO_TIMEOUT: Duration = Duration::from_secs(1);
const TRANSACTION_MAX_ATTEMPTS: usize = 8;
const MAX_EXTENSION_STATE_BYTES: u64 = crate::checkpoint::MAX_WIRE_INTEGER;

pub struct RedisCheckpointStore {
    connection: Mutex<Connection>,
    redis_url: String,
}

impl std::fmt::Debug for RedisCheckpointStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RedisCheckpointStore")
            .field("redis_url", &self.redis_url)
            .finish_non_exhaustive()
    }
}

impl RedisCheckpointStore {
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

    pub fn deferred_receipt_key(handle_key: &str) -> String {
        format!("{DEFERRED_RECEIPT_PREFIX}{handle_key}")
    }

    pub fn deferred_receipts_checkpoint_set_key(checkpoint_key: &str) -> String {
        let digest = Sha256::digest(checkpoint_key.as_bytes());
        format!("{DEFERRED_RECEIPTS_BY_CHECKPOINT_PREFIX}{digest:x}")
    }

    pub fn host_interaction_key(checkpoint_key: &str, interaction_id: &str) -> String {
        let digest = Sha256::digest(format!("{checkpoint_key}\0{interaction_id}").as_bytes());
        format!("{HOST_INTERACTION_PREFIX}{digest:x}")
    }

    pub fn host_interaction_notification_key(notification_id: &str) -> String {
        // The public contract keys notifications by their canonical
        // notification id, not by a storage record/id pair.  This keeps
        // replay and cross-process lookup identical to the other implementation and avoids a
        // second implicit identity derivation in Redis.
        let digest = Sha256::digest(notification_id.as_bytes());
        format!("{HOST_INTERACTION_NOTIFICATION_PREFIX}{digest:x}")
    }

    pub fn controller_command_key(command_id: &str) -> String {
        let digest = Sha256::digest(command_id.as_bytes());
        format!("{CONTROLLER_COMMAND_PREFIX}{digest:x}")
    }

    /// Canonical companion key for the closed command payload.
    ///
    /// The receipt, command payload, and wake outbox are intentionally
    /// separate Redis values.  This is the cross-language v8 layout used by
    /// The same closed layout is used by the other implementation; a receipt must never be decoded as an envelope that
    /// happens to contain the command again.
    pub fn controller_command_payload_key(command_id: &str) -> String {
        format!("{}:command", Self::controller_command_key(command_id))
    }

    pub fn controller_command_outbox_key(command_id: &str) -> String {
        format!("{}:outbox", Self::controller_command_key(command_id))
    }

    pub fn controller_receipts_checkpoint_set_key(checkpoint_key: &str) -> String {
        let digest = Sha256::digest(checkpoint_key.as_bytes());
        format!("{CONTROLLER_RECEIPTS_BY_CHECKPOINT_PREFIX}{digest:x}")
    }

    pub fn host_interactions_checkpoint_set_key(checkpoint_key: &str) -> String {
        let digest = Sha256::digest(checkpoint_key.as_bytes());
        format!("{HOST_INTERACTIONS_BY_CHECKPOINT_PREFIX}{digest:x}")
    }

    pub fn host_interaction_notifications_checkpoint_set_key(checkpoint_key: &str) -> String {
        let digest = Sha256::digest(checkpoint_key.as_bytes());
        format!("{HOST_INTERACTION_NOTIFICATIONS_BY_CHECKPOINT_PREFIX}{digest:x}")
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
    ) -> CheckpointResult<Option<Checkpoint>> {
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
                Some(value) => {
                    // MULTI/EXEC with no queued write commands returns an
                    // empty result (`None` for the typed query). Queue a
                    // harmless command so receipt replays still commit the
                    // WATCH snapshot without mutating checkpoint state.
                    pipeline.cmd("PING").ignore();
                    match pipeline.query::<Option<()>>(&mut *connection) {
                        Ok(Some(())) => {
                            redis::cmd("UNWATCH")
                                .query::<()>(&mut *connection)
                                .map_err(redis_error)?;
                            return Ok(value);
                        }
                        Ok(None) => continue,
                        Err(error) => return Err(redis_error(error)),
                    }
                }
            }
        }
        Err(CheckpointError::new(
            "checkpoint_store_transaction_retry_exhausted",
            "Redis checkpoint transaction retry limit exceeded",
        ))
    }

    fn receipt_transaction<T>(
        &self,
        data_key: &str,
        lease_key: &str,
        receipt_key: &str,
        receipt_set_key: &str,
        extra_watch_keys: &[&str],
        operation: impl Fn(&mut Connection, &mut Pipeline) -> CheckpointResult<Option<T>>,
    ) -> CheckpointResult<T> {
        let mut connection = self.lock()?;
        for _ in 0..TRANSACTION_MAX_ATTEMPTS {
            let mut watch = redis::cmd("WATCH");
            watch
                .arg(data_key)
                .arg(lease_key)
                .arg(receipt_key)
                .arg(receipt_set_key);
            for extra_watch_key in extra_watch_keys {
                watch.arg(*extra_watch_key);
            }
            watch.query::<()>(&mut *connection).map_err(redis_error)?;
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
                Some(value) => {
                    // A replay, not-admitted, or reconciliation decision may
                    // intentionally perform no writes.  Keep the WATCH/MULTI
                    // transaction observable so redis returns a committed
                    // result instead of an empty EXEC response.
                    pipeline.cmd("PING").ignore();
                    match pipeline.query::<Option<()>>(&mut *connection) {
                        Ok(Some(())) => {
                            redis::cmd("UNWATCH")
                                .query::<()>(&mut *connection)
                                .map_err(redis_error)?;
                            return Ok(value);
                        }
                        Ok(None) => continue,
                        Err(error) => return Err(redis_error(error)),
                    }
                }
            }
        }
        Err(CheckpointError::new(
            "checkpoint_store_transaction_retry_exhausted",
            "Redis deferred transaction retry limit exceeded",
        ))
    }

    fn controller_transaction<T>(
        &self,
        watch_keys: &[&str],
        operation: impl Fn(&mut Connection, &mut Pipeline) -> CheckpointResult<Option<T>>,
    ) -> CheckpointResult<T> {
        let mut connection = self.lock()?;
        for _ in 0..TRANSACTION_MAX_ATTEMPTS {
            let mut watch = redis::cmd("WATCH");
            for key in watch_keys {
                watch.arg(*key);
            }
            watch.query::<()>(&mut *connection).map_err(redis_error)?;
            let mut pipeline = redis::pipe();
            pipeline.atomic();
            match operation(&mut connection, &mut pipeline)? {
                None => {
                    redis::cmd("UNWATCH")
                        .query::<()>(&mut *connection)
                        .map_err(redis_error)?;
                    return Err(CheckpointError::new(
                        "checkpoint_store_conflict",
                        "controller operation did not match its compare-and-set precondition",
                    ));
                }
                Some(value) => {
                    pipeline.cmd("PING").ignore();
                    match pipeline.query::<Option<()>>(&mut *connection) {
                        Ok(Some(())) => {
                            redis::cmd("UNWATCH")
                                .query::<()>(&mut *connection)
                                .map_err(redis_error)?;
                            return Ok(value);
                        }
                        Ok(None) => continue,
                        Err(error) => return Err(redis_error(error)),
                    }
                }
            }
        }
        Err(CheckpointError::new(
            "checkpoint_store_transaction_retry_exhausted",
            "Redis controller transaction retry limit exceeded",
        ))
    }
}

include!("redis_impl_core.rs");
include!("redis_impl_tail.rs");

impl CheckpointStore for RedisCheckpointStore {
    redis_impl_core!();
    redis_impl_tail!();
}
include!("redis_checkpoint_methods.rs");
include!("redis_interaction.rs");
include!("redis_controller_wake.rs");
include!("redis_recovery.rs");
include!("redis_tail.rs");
