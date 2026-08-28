//! SQLite checkpoint v8 store.
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::checkpoint::{
    notification_id_for, record_id_for, CheckpointError, CheckpointResult, ClaimMode,
    ControllerCommand, ControllerCommandReceipt, ControllerCommandResolution,
    ControllerCommandVariant, ControllerCommandWake, EventCursor,
    HostInteractionNotificationPayload, HostInteractionNotificationRecord, HostInteractionOutcome,
    HostInteractionRecord, HostInteractionRecoveryEnvelope, HostInteractionRecoveryResult,
    HostInteractionRequest, HostInteractionResponse, NotificationOutboxState, ResumeObservation,
    SuspendedOrigin, HOST_INTERACTION_NOTIFICATION_SCHEMA, HOST_INTERACTION_RECORD_SCHEMA,
};
use crate::events::{EventId, RunEvent, RunEventPayload};
use crate::runtime::checkpoint_codec::{checkpoint_from_value, checkpoint_to_value};
use crate::runtime::state::{
    apply_claim, claim_candidate, prepare_ack, prepare_commit, prepare_event_delivery,
    prepare_finalize, prepare_finalize_claimed, prepare_progress, prepare_suspend, Checkpoint,
    CheckpointStore,
};
use crate::types::{AgentResult, CompletionReason};

#[path = "sqlite_deferred.rs"]
mod sqlite_deferred;
#[path = "sqlite_schema.rs"]
mod sqlite_schema;
const MAX_EXTENSION_STATE_BYTES: u64 = crate::checkpoint::MAX_WIRE_INTEGER;

pub struct SqliteCheckpointStore {
    connection: Mutex<Connection>,
    location: PathBuf,
}

impl std::fmt::Debug for SqliteCheckpointStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SqliteCheckpointStore")
            .field("location", &self.location)
            .finish_non_exhaustive()
    }
}

impl SqliteCheckpointStore {
    pub fn new(path: impl AsRef<Path>) -> CheckpointResult<Self> {
        let path = path.as_ref().to_path_buf();
        let connection = Connection::open(&path).map_err(sqlite_error)?;
        initialize_schema(&connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
            location: path,
        })
    }

    pub fn location(&self) -> &Path {
        &self.location
    }

    pub fn save_checkpoint(&self, checkpoint: Checkpoint) -> CheckpointResult<()> {
        checkpoint.validate()?;
        let values = SqlValues::from_checkpoint(&checkpoint)?;
        let connection = self.lock()?;
        connection
            .execute(
                r#"
                INSERT INTO checkpoints (
                    checkpoint_key, schema_version, run_definition_schema, run_definition,
                    task_id, root_run_id, trace_id, run_definition_digest, resume_attempt,
                    cycle_index, status, active_host_interaction, suspended_origin,
                    messages, cycles, model_calls, shared_state,
                    budget_usage, event_cursor, event_outbox, extension_state,
                    model_call_journal, tool_journal, revision, claim_token,
                    claimed_cycle, lease_expires_at_ms, terminal_result,
                    terminal_acknowledged
                ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                    ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26,
                    ?27, ?28, ?29
                )
                ON CONFLICT(checkpoint_key) DO UPDATE SET
                    schema_version = excluded.schema_version,
                    run_definition_schema = excluded.run_definition_schema,
                    run_definition = excluded.run_definition,
                    task_id = excluded.task_id,
                    root_run_id = excluded.root_run_id,
                    trace_id = excluded.trace_id,
                    run_definition_digest = excluded.run_definition_digest,
                    resume_attempt = excluded.resume_attempt,
                    cycle_index = excluded.cycle_index,
                    status = excluded.status,
                    active_host_interaction = excluded.active_host_interaction,
                    suspended_origin = excluded.suspended_origin,
                    messages = excluded.messages,
                    cycles = excluded.cycles,
                    model_calls = excluded.model_calls,
                    shared_state = excluded.shared_state,
                    budget_usage = excluded.budget_usage,
                    event_cursor = excluded.event_cursor,
                    event_outbox = excluded.event_outbox,
                    extension_state = excluded.extension_state,
                    model_call_journal = excluded.model_call_journal,
                    tool_journal = excluded.tool_journal,
                    revision = excluded.revision,
                    claim_token = excluded.claim_token,
                    claimed_cycle = excluded.claimed_cycle,
                    lease_expires_at_ms = excluded.lease_expires_at_ms,
                    terminal_result = excluded.terminal_result,
                    terminal_acknowledged = excluded.terminal_acknowledged
                "#,
                values.params(),
            )
            .map_err(sqlite_error)?;
        Ok(())
    }

    fn lock(&self) -> CheckpointResult<std::sync::MutexGuard<'_, Connection>> {
        self.connection.lock().map_err(|_| {
            CheckpointError::new(
                "checkpoint_store_lock_poisoned",
                "SQLite store lock poisoned",
            )
        })
    }
}

fn initialize_schema(connection: &Connection) -> CheckpointResult<()> {
    connection
        .execute_batch("PRAGMA foreign_keys=ON;")
        .map_err(sqlite_error)?;
    let expected_objects = [
        (
            "table",
            "checkpoints",
            sqlite_schema::CREATE_CHECKPOINTS_TABLE_SQL,
            "existing checkpoints table does not match the current schema; create a new database",
        ),
        (
            "index",
            "checkpoints_status_idx",
            sqlite_schema::CREATE_CHECKPOINTS_STATUS_INDEX_SQL,
            "existing checkpoints index does not match the current schema; create a new database",
        ),
        (
            "table",
            "deferred_resolution_receipts",
            sqlite_deferred::CREATE_DEFERRED_RECEIPTS_TABLE_SQL,
            "existing deferred receipt schema is incomplete; create a new database",
        ),
        (
            "index",
            "deferred_receipts_checkpoint_idx",
            sqlite_deferred::CREATE_DEFERRED_RECEIPTS_INDEX_SQL,
            "existing deferred receipt schema is incomplete; create a new database",
        ),
        (
            "table",
            "host_interaction_records",
            sqlite_schema::CREATE_HOST_INTERACTION_RECORDS_TABLE_SQL,
            "existing host interaction record schema is incomplete; create a new database",
        ),
        (
            "index",
            "host_interaction_records_checkpoint_idx",
            sqlite_schema::CREATE_HOST_INTERACTION_RECORDS_CHECKPOINT_INDEX_SQL,
            "existing host interaction record index is incomplete; create a new database",
        ),
        (
            "index",
            "host_interaction_records_recovery_idx",
            sqlite_schema::CREATE_HOST_INTERACTION_RECORDS_RECOVERY_INDEX_SQL,
            "existing host interaction recovery index is incomplete; create a new database",
        ),
        (
            "table",
            "host_interaction_notification_outbox",
            sqlite_schema::CREATE_HOST_INTERACTION_NOTIFICATION_TABLE_SQL,
            "existing host interaction notification schema is incomplete; create a new database",
        ),
        (
            "index",
            "host_interaction_notification_outbox_checkpoint_idx",
            sqlite_schema::CREATE_HOST_INTERACTION_NOTIFICATION_CHECKPOINT_INDEX_SQL,
            "existing host interaction notification index is incomplete; create a new database",
        ),
        (
            "index",
            "host_interaction_notification_outbox_lease_idx",
            sqlite_schema::CREATE_HOST_INTERACTION_NOTIFICATION_LEASE_INDEX_SQL,
            "existing host interaction notification lease index is incomplete; create a new database",
        ),
        (
            "table",
            "controller_command_receipts",
            sqlite_schema::CREATE_CONTROLLER_RECEIPTS_TABLE_SQL,
            "existing controller receipt schema is incomplete; create a new database",
        ),
        (
            "index",
            "controller_command_receipts_checkpoint_idx",
            sqlite_schema::CREATE_CONTROLLER_RECEIPTS_CHECKPOINT_INDEX_SQL,
            "existing controller receipt index is incomplete; create a new database",
        ),
        (
            "index",
            "controller_command_receipts_outbox_idx",
            sqlite_schema::CREATE_CONTROLLER_RECEIPTS_OUTBOX_INDEX_SQL,
            "existing controller receipt outbox index is incomplete; create a new database",
        ),
    ];
    // Probe every canonical name before deciding whether this is a fresh database.  In
    // particular, do not let an early match skip probing a later name: a later
    // case-insensitive collision must still make schema validation fail closed.
    let schema_matches = expected_objects
        .iter()
        .map(|(_, name, _, _)| schema_objects(connection, name))
        .collect::<CheckpointResult<Vec<_>>>()?;
    let has_related_objects = schema_matches.iter().any(|objects| !objects.is_empty());

    if has_related_objects {
        for (objects, (expected_type, _, expected_sql, message)) in
            schema_matches.iter().zip(expected_objects.iter())
        {
            validate_schema_object(objects, expected_type, expected_sql, message)?;
        }
    } else {
        connection
            .execute_batch(sqlite_schema::CREATE_CHECKPOINTS_TABLE_SQL)
            .map_err(sqlite_error)?;
        connection
            .execute_batch(sqlite_schema::CREATE_CHECKPOINTS_STATUS_INDEX_SQL)
            .map_err(sqlite_error)?;
        connection
            .execute_batch(sqlite_deferred::CREATE_DEFERRED_RECEIPTS_TABLE_SQL)
            .map_err(sqlite_error)?;
        connection
            .execute_batch(sqlite_deferred::CREATE_DEFERRED_RECEIPTS_INDEX_SQL)
            .map_err(sqlite_error)?;
        connection
            .execute_batch(sqlite_schema::CREATE_HOST_INTERACTION_RECORDS_TABLE_SQL)
            .map_err(sqlite_error)?;
        connection
            .execute_batch(sqlite_schema::CREATE_HOST_INTERACTION_RECORDS_CHECKPOINT_INDEX_SQL)
            .map_err(sqlite_error)?;
        connection
            .execute_batch(sqlite_schema::CREATE_HOST_INTERACTION_RECORDS_RECOVERY_INDEX_SQL)
            .map_err(sqlite_error)?;
        connection
            .execute_batch(sqlite_schema::CREATE_HOST_INTERACTION_NOTIFICATION_TABLE_SQL)
            .map_err(sqlite_error)?;
        connection
            .execute_batch(sqlite_schema::CREATE_HOST_INTERACTION_NOTIFICATION_CHECKPOINT_INDEX_SQL)
            .map_err(sqlite_error)?;
        connection
            .execute_batch(sqlite_schema::CREATE_HOST_INTERACTION_NOTIFICATION_LEASE_INDEX_SQL)
            .map_err(sqlite_error)?;
        connection
            .execute_batch(sqlite_schema::CREATE_CONTROLLER_RECEIPTS_TABLE_SQL)
            .map_err(sqlite_error)?;
        connection
            .execute_batch(sqlite_schema::CREATE_CONTROLLER_RECEIPTS_CHECKPOINT_INDEX_SQL)
            .map_err(sqlite_error)?;
        connection
            .execute_batch(sqlite_schema::CREATE_CONTROLLER_RECEIPTS_OUTBOX_INDEX_SQL)
            .map_err(sqlite_error)?;
    }
    connection
        .execute_batch("PRAGMA journal_mode=WAL;")
        .map_err(sqlite_error)?;
    Ok(())
}

#[derive(Debug)]
struct SchemaObject {
    object_type: String,
    sql: Option<String>,
}

fn schema_objects(connection: &Connection, name: &str) -> CheckpointResult<Vec<SchemaObject>> {
    let mut statement = connection
        .prepare("SELECT type, sql FROM sqlite_master WHERE lower(name) = lower(?1)")
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![name], |row| {
            Ok(SchemaObject {
                object_type: row.get(0)?,
                sql: row.get(1)?,
            })
        })
        .map_err(sqlite_error)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sqlite_error)
}

fn validate_schema_object(
    objects: &[SchemaObject],
    expected_type: &str,
    expected_sql: &str,
    message: &str,
) -> CheckpointResult<()> {
    if objects.len() != 1
        || objects[0].object_type != expected_type
        || objects[0].sql.as_deref().map(normalize_schema_sql)
            != Some(normalize_schema_sql(expected_sql))
    {
        return Err(schema_mismatch(message));
    }
    Ok(())
}

fn normalize_schema_sql(sql: &str) -> String {
    sql.replace("IF NOT EXISTS", "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn schema_mismatch(message: &str) -> CheckpointError {
    CheckpointError::new("checkpoint_store_schema_mismatch", message)
}

impl CheckpointStore for SqliteCheckpointStore {
    fn store_identity(&self) -> String {
        let location = self
            .location
            .canonicalize()
            .unwrap_or_else(|_| self.location.clone());
        format!("sqlite:{}", location.to_string_lossy())
    }

    fn create_checkpoint(&self, checkpoint: Checkpoint) -> CheckpointResult<bool> {
        checkpoint.validate()?;
        let values = SqlValues::from_checkpoint(&checkpoint)?;
        let connection = self.lock()?;
        let changed = connection
            .execute(
                r#"
                INSERT OR IGNORE INTO checkpoints (
                    checkpoint_key, schema_version, run_definition_schema, run_definition,
                    task_id, root_run_id, trace_id, run_definition_digest, resume_attempt,
                    cycle_index, status, active_host_interaction, suspended_origin,
                    messages, cycles, model_calls, shared_state,
                    budget_usage, event_cursor, event_outbox, extension_state,
                    model_call_journal, tool_journal, revision, claim_token,
                    claimed_cycle, lease_expires_at_ms, terminal_result,
                    terminal_acknowledged
                ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                    ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26,
                    ?27, ?28, ?29
                )
                "#,
                values.params(),
            )
            .map_err(sqlite_error)?;
        Ok(changed == 1)
    }

    fn load_checkpoint(&self, checkpoint_key: &str) -> CheckpointResult<Option<Checkpoint>> {
        let connection = self.lock()?;
        load_row(&connection, checkpoint_key)
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
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite_error)?;
        let Some(current) = load_row_transaction(&transaction, checkpoint_key)? else {
            transaction.commit().map_err(sqlite_error)?;
            return Ok(None);
        };
        if !claim_candidate(&current, cycle_index, now_ms, claim_mode)? {
            transaction.commit().map_err(sqlite_error)?;
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
        let values = SqlValues::from_checkpoint(&claimed)?;
        let changed = update_row(&transaction, &values, Some(claimed.revision - 1), None)?;
        transaction.commit().map_err(sqlite_error)?;
        if changed {
            Ok(Some(claimed))
        } else {
            Ok(None)
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
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite_error)?;
        let Some(current) = load_row_transaction(&transaction, &checkpoint.checkpoint_key)? else {
            transaction.commit().map_err(sqlite_error)?;
            return Ok(false);
        };
        let Some(updated) = prepare_finalize(&current, checkpoint, expected_revision)? else {
            transaction.commit().map_err(sqlite_error)?;
            return Ok(false);
        };
        let values = SqlValues::from_checkpoint(&updated)?;
        let changed = update_row(&transaction, &values, Some(expected_revision), None)?;
        transaction.commit().map_err(sqlite_error)?;
        Ok(changed)
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
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite_error)?;
        let Some(current) = load_row_transaction(&transaction, checkpoint_key)? else {
            transaction.commit().map_err(sqlite_error)?;
            return Ok(false);
        };
        if current.claim_token.as_deref() != Some(claim_token)
            || current
                .lease_expires_at_ms
                .is_none_or(|expiry| expiry <= now_ms)
        {
            transaction.commit().map_err(sqlite_error)?;
            return Ok(false);
        }
        let changed = transaction
            .execute(
                "UPDATE checkpoints SET lease_expires_at_ms = ?1 WHERE checkpoint_key = ?2 AND claim_token = ?3 AND lease_expires_at_ms > ?4",
                params![
                    to_i64(lease_expires_at_ms, "lease_expires_at_ms")?,
                    checkpoint_key,
                    claim_token,
                    to_i64(now_ms, "now_ms")?
                ],
            )
            .map_err(sqlite_error)?;
        transaction.commit().map_err(sqlite_error)?;
        Ok(changed == 1)
    }

    fn acknowledge_terminal(
        &self,
        checkpoint_key: &str,
        expected_revision: u64,
    ) -> CheckpointResult<bool> {
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite_error)?;
        let Some(current) = load_row_transaction(&transaction, checkpoint_key)? else {
            transaction.commit().map_err(sqlite_error)?;
            return Ok(false);
        };
        let Some(updated) = prepare_ack(&current, expected_revision)? else {
            transaction.commit().map_err(sqlite_error)?;
            return Ok(false);
        };
        let values = SqlValues::from_checkpoint(&updated)?;
        let changed = update_row(&transaction, &values, Some(expected_revision), None)?;
        transaction.commit().map_err(sqlite_error)?;
        Ok(changed)
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
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite_error)?;
        let Some(current) = load_row_transaction(&transaction, checkpoint_key)? else {
            transaction.commit().map_err(sqlite_error)?;
            return Ok(false);
        };
        let Some(updated) = prepare_event_delivery(
            &current,
            claim_token,
            expected_revision,
            event_id,
            payload_digest,
            cursor,
        )?
        else {
            transaction.commit().map_err(sqlite_error)?;
            return Ok(false);
        };
        let values = SqlValues::from_checkpoint(&updated)?;
        let changed = update_row(&transaction, &values, Some(expected_revision), claim_token)?;
        transaction.commit().map_err(sqlite_error)?;
        Ok(changed)
    }

    fn delete_checkpoint(&self, checkpoint_key: &str) -> CheckpointResult<()> {
        let connection = self.lock()?;
        connection
            .execute(
                "DELETE FROM checkpoints WHERE checkpoint_key = ?1",
                params![checkpoint_key],
            )
            .map_err(sqlite_error)?;
        Ok(())
    }

    fn list_checkpoints(&self) -> CheckpointResult<Vec<String>> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare("SELECT checkpoint_key FROM checkpoints ORDER BY checkpoint_key")
            .map_err(sqlite_error)?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(sqlite_error)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_error)
    }

    fn admit_deferred_batch(
        &self,
        checkpoint_key: &str,
        expected_revision: u64,
        claim_token: &str,
        claimed_cycle: u64,
        entries: &[crate::checkpoint::DeferredBatchEntry],
    ) -> CheckpointResult<crate::checkpoint::DeferredBatchAdmission> {
        sqlite_deferred::admit_deferred_batch(
            self,
            checkpoint_key,
            expected_revision,
            claim_token,
            claimed_cycle,
            entries,
        )
    }

    fn resolve_deferred(
        &self,
        handle: crate::checkpoint::DeferredToolHandle,
        result: crate::types::ToolExecutionResult,
    ) -> CheckpointResult<crate::checkpoint::DeferredResolveDecision> {
        sqlite_deferred::resolve_deferred(self, handle, result)
    }

    fn accept_deferred_batch(
        &self,
        checkpoint_key: &str,
        expected_revision: u64,
        claim_token: &str,
        claimed_cycle: u64,
        decisions: &[crate::checkpoint::AcceptDeferredDecision],
    ) -> CheckpointResult<crate::checkpoint::DeferredBatchAdmission> {
        sqlite_deferred::accept_deferred_batch(
            self,
            checkpoint_key,
            expected_revision,
            claim_token,
            claimed_cycle,
            decisions,
        )
    }

    fn produce_host_interaction(
        &self,
        request: HostInteractionRequest,
        context: &crate::checkpoint::HostInteractionAdmissionContext,
    ) -> CheckpointResult<HostInteractionOutcome> {
        sqlite_produce_host_interaction(self, request, context)
    }

    fn admit_controller_command(
        &self,
        command: ControllerCommand,
    ) -> CheckpointResult<ControllerCommandReceipt> {
        sqlite_admit_controller_command(self, command)
    }

    fn resolve_controller_command(
        &self,
        command: ControllerCommand,
    ) -> CheckpointResult<ControllerCommandResolution> {
        sqlite_resolve_controller_command(self, command)
    }

    fn get_controller_command_receipt(
        &self,
        command_id: &str,
    ) -> CheckpointResult<Option<ControllerCommandReceipt>> {
        sqlite_get_controller_command_receipt(self, command_id)
    }

    fn get_controller_command(
        &self,
        command_id: &str,
    ) -> CheckpointResult<Option<ControllerCommand>> {
        sqlite_get_controller_command(self, command_id)
    }

    fn claim_and_consume_host_interaction_response(
        &self,
        envelope: HostInteractionRecoveryEnvelope,
    ) -> CheckpointResult<HostInteractionRecoveryResult> {
        sqlite_claim_and_consume_host_interaction_response(self, envelope)
    }

    fn reap_host_interaction_record(
        &self,
        record_id: &str,
        checkpoint_key: &str,
        now_ms: u64,
    ) -> CheckpointResult<bool> {
        sqlite_reap_host_interaction_record(self, record_id, checkpoint_key, now_ms)
    }

    fn claim_host_interaction_notification(
        &self,
        notification_id: &str,
        payload_digest: &str,
        claim_token: &str,
        lease_expires_at_ms: u64,
        now_ms: u64,
    ) -> CheckpointResult<Option<HostInteractionNotificationRecord>> {
        sqlite_claim_host_interaction_notification(
            self,
            notification_id,
            payload_digest,
            claim_token,
            lease_expires_at_ms,
            now_ms,
        )
    }

    fn get_host_interaction_notification(
        &self,
        notification_id: &str,
    ) -> CheckpointResult<Option<HostInteractionNotificationRecord>> {
        sqlite_get_host_interaction_notification(self, notification_id)
    }

    fn complete_host_interaction_notification(
        &self,
        notification_id: &str,
        payload_digest: &str,
        claim_token: &str,
        attempt: u64,
        outcome: &str,
        now_ms: u64,
        error: Option<&str>,
    ) -> CheckpointResult<Option<HostInteractionNotificationRecord>> {
        sqlite_complete_host_interaction_notification(
            self,
            notification_id,
            payload_digest,
            claim_token,
            attempt,
            outcome,
            now_ms,
            error,
        )
    }

    fn reconcile_host_interaction_notification(
        &self,
        notification_id: &str,
        payload_digest: &str,
        outcome: &str,
        now_ms: u64,
        abort_reason: Option<&str>,
    ) -> CheckpointResult<Option<HostInteractionNotificationRecord>> {
        sqlite_reconcile_host_interaction_notification(
            self,
            notification_id,
            payload_digest,
            outcome,
            now_ms,
            abort_reason,
        )
    }

    fn claim_controller_command_wake(
        &self,
        command_id: &str,
        command_digest: &str,
        claim_token: &str,
        lease_expires_at_ms: u64,
        now_ms: u64,
    ) -> CheckpointResult<Option<ControllerCommandReceipt>> {
        sqlite_claim_controller_command_wake(
            self,
            command_id,
            command_digest,
            claim_token,
            lease_expires_at_ms,
            now_ms,
        )
    }

    fn complete_controller_command_wake(
        &self,
        command_id: &str,
        command_digest: &str,
        claim_token: &str,
        attempt: u64,
        outcome: &str,
        now_ms: u64,
        error: Option<&str>,
    ) -> CheckpointResult<Option<ControllerCommandReceipt>> {
        sqlite_complete_controller_command_wake(
            self,
            command_id,
            command_digest,
            claim_token,
            attempt,
            outcome,
            now_ms,
            error,
        )
    }

    fn reconcile_controller_command_wake(
        &self,
        command_id: &str,
        command_digest: &str,
        outcome: &str,
        now_ms: u64,
    ) -> CheckpointResult<Option<ControllerCommandReceipt>> {
        sqlite_reconcile_controller_command_wake(self, command_id, command_digest, outcome, now_ms)
    }

    fn reap_controller_command_wake(
        &self,
        command_id: &str,
        command_digest: &str,
        now_ms: u64,
    ) -> CheckpointResult<bool> {
        sqlite_reap_controller_command_wake(self, command_id, command_digest, now_ms)
    }
}

include!("sqlite_interaction.rs");
include!("sqlite_recovery.rs");
include!("sqlite_rows.rs");
