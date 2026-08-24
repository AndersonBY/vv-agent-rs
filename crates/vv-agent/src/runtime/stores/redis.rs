//! Redis checkpoint store.

use std::sync::Mutex;
use std::time::Duration;

use redis::{Commands, Connection, Pipeline};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::checkpoint::{CheckpointError, CheckpointResult, ClaimMode, EventCursor};
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
        operation: impl Fn(&mut Connection, &mut Pipeline) -> CheckpointResult<Option<T>>,
    ) -> CheckpointResult<T> {
        let mut connection = self.lock()?;
        for _ in 0..TRANSACTION_MAX_ATTEMPTS {
            redis::cmd("WATCH")
                .arg(data_key)
                .arg(lease_key)
                .arg(receipt_key)
                .arg(receipt_set_key)
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
}

impl CheckpointStore for RedisCheckpointStore {
    fn store_identity(&self) -> String {
        format!("redis:{}", self.redis_url)
    }

    fn create_checkpoint(&self, checkpoint: Checkpoint) -> CheckpointResult<bool> {
        checkpoint.validate()?;
        let data_key = Self::data_key(&checkpoint.checkpoint_key);
        let lease_key = Self::lease_key(&checkpoint.checkpoint_key);
        let payload = checkpoint_to_json(&checkpoint, MAX_EXTENSION_STATE_BYTES)?;
        let mut connection = self.lock()?;
        let created: bool = connection.set_nx(&data_key, payload).map_err(redis_error)?;
        if created {
            connection.del::<_, ()>(&lease_key).map_err(redis_error)?;
        }
        Ok(created)
    }

    fn load_checkpoint(&self, checkpoint_key: &str) -> CheckpointResult<Option<Checkpoint>> {
        let data_key = Self::data_key(checkpoint_key);
        let lease_key = Self::lease_key(checkpoint_key);
        let mut connection = self.lock()?;
        Self::load_from_connection(&mut connection, &data_key, &lease_key)
    }

    fn claim_checkpoint(
        &self,
        checkpoint_key: &str,
        cycle_index: u64,
        claim_token: &str,
        lease_expires_at_ms: u64,
        now_ms: u64,
        claim_mode: ClaimMode,
    ) -> CheckpointResult<Option<Checkpoint>> {
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
            let payload = checkpoint_to_json(&claimed, MAX_EXTENSION_STATE_BYTES)?;
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

    fn progress_checkpoint(
        &self,
        checkpoint: Checkpoint,
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

    fn suspend_checkpoint(
        &self,
        checkpoint: Checkpoint,
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

    fn commit_checkpoint(
        &self,
        checkpoint: Checkpoint,
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

    fn finalize_claimed_checkpoint(
        &self,
        checkpoint: Checkpoint,
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

    fn finalize_checkpoint(
        &self,
        checkpoint: Checkpoint,
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
            let payload = checkpoint_to_json(&updated, MAX_EXTENSION_STATE_BYTES)?;
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

    fn renew_checkpoint_claim(
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

    fn acknowledge_terminal(
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
            let payload = checkpoint_to_json(&updated, MAX_EXTENSION_STATE_BYTES)?;
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

    fn record_event_delivery(
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
            let payload = checkpoint_to_json(&updated, MAX_EXTENSION_STATE_BYTES)?;
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

    fn admit_deferred_batch(
        &self,
        checkpoint_key: &str,
        expected_revision: u64,
        claim_token: &str,
        claimed_cycle: u64,
        entries: &[crate::checkpoint::DeferredBatchEntry],
    ) -> CheckpointResult<crate::checkpoint::DeferredBatchAdmission> {
        let data_key = Self::data_key(checkpoint_key);
        let lease_key = Self::lease_key(checkpoint_key);
        let result = self.transaction(&data_key, &lease_key, |connection, pipeline| {
            let Some(raw) = connection
                .get::<_, Option<String>>(&data_key)
                .map_err(redis_error)?
            else {
                return Err(CheckpointError::new(
                    "checkpoint_not_found",
                    "checkpoint does not exist",
                ));
            };
            let current = decode_storage(
                &raw,
                connection
                    .get::<_, Option<u64>>(&lease_key)
                    .map_err(redis_error)?,
            )?;
            let (updated, handles) = crate::runtime::state::admit_deferred_batch(
                &current,
                expected_revision,
                claim_token,
                claimed_cycle,
                entries,
            )?;
            if updated.revision == current.revision {
                return Ok(None);
            }
            let payload = checkpoint_to_json(&updated, MAX_EXTENSION_STATE_BYTES)?;
            pipeline.set(&data_key, payload).ignore();
            pipeline.del(&lease_key).ignore();
            Ok(Some(crate::checkpoint::DeferredBatchAdmission {
                checkpoint: updated,
                handles,
            }))
        });
        match result {
            Ok(value) => Ok(value),
            Err(error) if error.code() == "checkpoint_store_conflict" => Err(CheckpointError::new(
                "checkpoint_revision_conflict",
                "deferred batch admission compare-and-swap failed",
            )),
            Err(error) => Err(error),
        }
    }

    fn resolve_deferred(
        &self,
        handle: crate::checkpoint::DeferredToolHandle,
        result: crate::types::ToolExecutionResult,
    ) -> CheckpointResult<crate::checkpoint::DeferredResolveDecision> {
        use crate::checkpoint::{DeferredReceipt, DeferredResolveDecision, OperationState};
        crate::checkpoint::validate_definitive_result(&result)?;
        let handle_key = handle.handle_key()?;
        let data_key = Self::data_key(&handle.checkpoint_key);
        let lease_key = Self::lease_key(&handle.checkpoint_key);
        let receipt_key = Self::deferred_receipt_key(&handle_key);
        let receipt_set_key = Self::deferred_receipts_checkpoint_set_key(&handle.checkpoint_key);
        let result_digest = crate::checkpoint::result_digest(&result)?;
        let outcome = self.receipt_transaction(
            &data_key,
            &lease_key,
            &receipt_key,
            &receipt_set_key,
            |connection, pipeline| {
                if let Some(raw_receipt) = connection
                    .get::<_, Option<String>>(&receipt_key)
                    .map_err(redis_error)?
                {
                    let receipt = decode_receipt(&raw_receipt)?;
                    if receipt.result_digest == result_digest {
                        return Ok(Some(DeferredResolveDecision::Replayed { receipt }));
                    }
                    return Err(CheckpointError::new(
                        "deferred_resolution_conflict",
                        "deferred handle already has a different definitive result",
                    ));
                }
                let Some(raw) = connection
                    .get::<_, Option<String>>(&data_key)
                    .map_err(redis_error)?
                else {
                    return Err(CheckpointError::new(
                        "deferred_resolution_stale",
                        "checkpoint does not exist",
                    ));
                };
                let checkpoint = decode_storage(
                    &raw,
                    connection
                        .get::<_, Option<u64>>(&lease_key)
                        .map_err(redis_error)?,
                )?;
                let Some(index) = checkpoint.tool_journal.iter().position(|entry| {
                    entry.operation_id == handle.operation_id
                        && entry.attempt == handle.attempt
                        && entry.request_digest == handle.request_digest
                }) else {
                    return Err(CheckpointError::new(
                        "deferred_resolution_stale",
                        "no active journal matches the deferred handle",
                    ));
                };
                let entry = &checkpoint.tool_journal[index];
                if entry.state == OperationState::Started {
                    return Ok(Some(DeferredResolveDecision::not_admitted()));
                }
                if entry.state == OperationState::Ambiguous {
                    return Ok(Some(DeferredResolveDecision::ReconciliationRequired));
                }
                if checkpoint.claim_token.is_some() {
                    return Err(CheckpointError::new(
                        "deferred_checkpoint_claimed",
                        "deferred resolution is blocked while the checkpoint is claimed",
                    ));
                }
                if entry.state != OperationState::Deferred
                    || entry.deferred_handle.as_ref() != Some(&handle)
                {
                    return Err(CheckpointError::new(
                        "deferred_resolution_stale",
                        "deferred handle is stale or no longer active",
                    ));
                }
                if entry.tool_call_id.as_deref() != Some(result.tool_call_id.as_str()) {
                    return Err(CheckpointError::new(
                        "deferred_resolution_stale",
                        "deferred result tool_call_id does not match the journal",
                    ));
                }
                let event = crate::runtime::state::receipt_event(&checkpoint, entry, &result)?;
                let event_id = event.event_id.clone();
                let event_digest = event.payload_digest.clone();
                let mut updated = checkpoint.clone();
                let journal = &mut updated.tool_journal[index];
                match result.status {
                    crate::types::ToolResultStatus::Success => {
                        journal.state = OperationState::Succeeded;
                        journal.result = Some(result.to_dict());
                    }
                    crate::types::ToolResultStatus::Error => {
                        journal.state = OperationState::Failed;
                        journal.error = Some(crate::runtime::state::OperationError::new(
                            result
                                .error_code
                                .clone()
                                .unwrap_or_else(|| "tool_error".to_string()),
                            result.content.clone(),
                            false,
                        ));
                    }
                    _ => unreachable!(),
                }
                journal.deferred_handle = None;
                updated.event_outbox.push(event);
                let unresolved = updated
                    .tool_journal
                    .iter()
                    .any(|entry| entry.state == OperationState::Deferred);
                updated.status = if unresolved {
                    crate::checkpoint::CheckpointStatus::Deferred
                } else {
                    crate::checkpoint::CheckpointStatus::Running
                };
                updated.revision = checkpoint.revision.checked_add(1).ok_or_else(|| {
                    CheckpointError::new("checkpoint_revision_overflow", "revision overflow")
                })?;
                updated.validate()?;
                let receipt =
                    DeferredReceipt::new(handle.clone(), result.clone(), event_id, event_digest)?;
                let payload = checkpoint_to_json(&updated, MAX_EXTENSION_STATE_BYTES)?;
                let receipt_payload = encode_receipt(&receipt)?;
                pipeline.set(&data_key, payload).ignore();
                pipeline.del(&lease_key).ignore();
                pipeline.set(&receipt_key, receipt_payload).ignore();
                pipeline.sadd(&receipt_set_key, &receipt_key).ignore();
                Ok(Some(if unresolved {
                    DeferredResolveDecision::AppliedWaiting { receipt }
                } else {
                    DeferredResolveDecision::AppliedReady { receipt }
                }))
            },
        )?;
        Ok(outcome)
    }

    fn accept_deferred_batch(
        &self,
        checkpoint_key: &str,
        expected_revision: u64,
        claim_token: &str,
        claimed_cycle: u64,
        decisions: &[crate::checkpoint::AcceptDeferredDecision],
    ) -> CheckpointResult<crate::checkpoint::DeferredBatchAdmission> {
        let data_key = Self::data_key(checkpoint_key);
        let lease_key = Self::lease_key(checkpoint_key);
        let result = self.transaction(&data_key, &lease_key, |connection, pipeline| {
            let Some(raw) = connection
                .get::<_, Option<String>>(&data_key)
                .map_err(redis_error)?
            else {
                return Err(CheckpointError::new(
                    "checkpoint_not_found",
                    "checkpoint does not exist",
                ));
            };
            let current = decode_storage(
                &raw,
                connection
                    .get::<_, Option<u64>>(&lease_key)
                    .map_err(redis_error)?,
            )?;
            if crate::runtime::state::deferred_batch_is_idempotent(&current, decisions) {
                return Ok(Some(crate::checkpoint::DeferredBatchAdmission {
                    checkpoint: current,
                    handles: decisions
                        .iter()
                        .map(|decision| decision.handle.clone())
                        .collect(),
                }));
            }
            let (updated, changed) = crate::runtime::state::accept_deferred_batch(
                &current,
                expected_revision,
                claim_token,
                claimed_cycle,
                decisions,
            )?;
            if !changed {
                return Ok(None);
            }
            let payload = checkpoint_to_json(&updated, MAX_EXTENSION_STATE_BYTES)?;
            pipeline.set(&data_key, payload).ignore();
            pipeline.del(&lease_key).ignore();
            Ok(Some(crate::checkpoint::DeferredBatchAdmission {
                checkpoint: updated,
                handles: decisions
                    .iter()
                    .map(|decision| decision.handle.clone())
                    .collect(),
            }))
        });
        match result {
            Ok(value) => Ok(value),
            Err(error) if error.code() == "checkpoint_store_conflict" => Err(CheckpointError::new(
                "reconciliation_required",
                "deferred reconciliation compare-and-swap failed",
            )),
            Err(error) => Err(error),
        }
    }

    fn delete_checkpoint(&self, checkpoint_key: &str) -> CheckpointResult<()> {
        let mut connection = self.lock()?;
        let data_key = Self::data_key(checkpoint_key);
        let lease_key = Self::lease_key(checkpoint_key);
        let receipt_set_key = Self::deferred_receipts_checkpoint_set_key(checkpoint_key);
        for _ in 0..TRANSACTION_MAX_ATTEMPTS {
            // Resolve watches both the checkpoint and the receipt index.  A
            // resolver that races this cleanup must invalidate EXEC before it
            // can add a receipt to the set, so no orphan receipt survives.
            redis::cmd("WATCH")
                .arg(&data_key)
                .arg(&lease_key)
                .arg(&receipt_set_key)
                .query::<()>(&mut *connection)
                .map_err(redis_error)?;
            let receipt_keys: Vec<String> =
                connection.smembers(&receipt_set_key).map_err(redis_error)?;
            for receipt_key in &receipt_keys {
                redis::cmd("WATCH")
                    .arg(receipt_key)
                    .query::<()>(&mut *connection)
                    .map_err(redis_error)?;
            }
            let mut keys = vec![data_key.clone(), lease_key.clone(), receipt_set_key.clone()];
            keys.extend(receipt_keys);
            let mut pipeline = redis::pipe();
            pipeline.atomic().del(keys).cmd("PING").ignore();
            match pipeline.query::<Option<()>>(&mut *connection) {
                Ok(Some(())) => {
                    redis::cmd("UNWATCH")
                        .query::<()>(&mut *connection)
                        .map_err(redis_error)?;
                    return Ok(());
                }
                Ok(None) => continue,
                Err(error) => return Err(redis_error(error)),
            }
        }
        Err(CheckpointError::new(
            "checkpoint_store_transaction_retry_exhausted",
            "Redis checkpoint cleanup retry limit exceeded",
        ))
    }

    fn list_checkpoints(&self) -> CheckpointResult<Vec<String>> {
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

impl RedisCheckpointStore {
    fn replace_claimed(
        &self,
        checkpoint: Checkpoint,
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
            let payload = checkpoint_to_json(&updated, MAX_EXTENSION_STATE_BYTES)?;
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

fn decode_storage(raw: &str, lease: Option<u64>) -> CheckpointResult<Checkpoint> {
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
    checkpoint_from_json(&payload, MAX_EXTENSION_STATE_BYTES)
}

fn encode_receipt(receipt: &crate::checkpoint::DeferredReceipt) -> CheckpointResult<String> {
    receipt.validate()?;
    serde_json::to_string(receipt)
        .map_err(|error| CheckpointError::new("deferred_receipt_invalid", error.to_string()))
}

fn decode_receipt(raw: &str) -> CheckpointResult<crate::checkpoint::DeferredReceipt> {
    let receipt: crate::checkpoint::DeferredReceipt = serde_json::from_str(raw)
        .map_err(|error| CheckpointError::new("deferred_receipt_invalid", error.to_string()))?;
    receipt.validate()?;
    Ok(receipt)
}

fn redis_error(error: redis::RedisError) -> CheckpointError {
    CheckpointError::new("checkpoint_store_redis", error.to_string())
}
