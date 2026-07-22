//! SQLite checkpoint v2 store.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::{Map, Value};

use crate::checkpoint::{CheckpointError, CheckpointResult, ClaimMode, EventCursor};
use crate::runtime::checkpoint_codec::{checkpoint_from_value, checkpoint_to_value};
use crate::runtime::state::{
    apply_claim, claim_candidate, prepare_ack, prepare_commit, prepare_event_delivery,
    prepare_finalize, prepare_finalize_claimed, prepare_progress, prepare_suspend, Checkpoint,
    CheckpointStore,
};

const MAX_EXTENSION_STATE_BYTES: u64 = crate::checkpoint::MAX_WIRE_INTEGER;
const CREATE_CHECKPOINTS_TABLE_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS checkpoints (
    checkpoint_key TEXT PRIMARY KEY,
    schema_version TEXT NOT NULL CHECK (schema_version = 'vv-agent.checkpoint.v2'),
    run_definition_schema TEXT NOT NULL CHECK (run_definition_schema = 'vv-agent.run-definition.v1'),
    run_definition TEXT NOT NULL,
    task_id TEXT NOT NULL,
    root_run_id TEXT NOT NULL,
    trace_id TEXT NOT NULL,
    run_definition_digest TEXT NOT NULL,
    resume_attempt INTEGER NOT NULL CHECK (resume_attempt >= 1),
    cycle_index INTEGER NOT NULL CHECK (cycle_index >= 0),
    status TEXT NOT NULL,
    messages TEXT NOT NULL,
    cycles TEXT NOT NULL,
    shared_state TEXT NOT NULL,
    budget_usage TEXT,
    event_cursor TEXT,
    event_outbox TEXT NOT NULL,
    extension_state TEXT NOT NULL,
    model_call_journal TEXT NOT NULL,
    tool_journal TEXT NOT NULL,
    revision INTEGER NOT NULL DEFAULT 0 CHECK (revision >= 0),
    claim_token TEXT,
    claimed_cycle INTEGER,
    lease_expires_at_ms INTEGER,
    terminal_result TEXT,
    terminal_acknowledged INTEGER NOT NULL DEFAULT 0 CHECK (terminal_acknowledged IN (0, 1)),
    CHECK (
        (claim_token IS NULL AND claimed_cycle IS NULL AND lease_expires_at_ms IS NULL)
        OR
        (claim_token IS NOT NULL AND claimed_cycle IS NOT NULL AND lease_expires_at_ms IS NOT NULL)
    ),
    CHECK (claim_token IS NULL OR claimed_cycle = cycle_index + 1),
    CHECK (terminal_result IS NULL OR claim_token IS NULL)
)
"#;
const CREATE_CHECKPOINTS_STATUS_INDEX_SQL: &str = r#"
CREATE INDEX IF NOT EXISTS checkpoints_status_idx ON checkpoints(status)
"#;

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
                    cycle_index, status, messages, cycles, shared_state, budget_usage,
                    event_cursor, event_outbox, extension_state, model_call_journal,
                    tool_journal, revision, claim_token, claimed_cycle,
                    lease_expires_at_ms, terminal_result, terminal_acknowledged
                ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                    ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26
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
                    messages = excluded.messages,
                    cycles = excluded.cycles,
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
        .execute_batch("PRAGMA journal_mode=WAL;")
        .map_err(sqlite_error)?;
    match schema_sql(connection, "table", "checkpoints")? {
        None => {
            connection
                .execute_batch(CREATE_CHECKPOINTS_TABLE_SQL)
                .map_err(sqlite_error)?;
            connection
                .execute_batch(CREATE_CHECKPOINTS_STATUS_INDEX_SQL)
                .map_err(sqlite_error)?;
        }
        Some(existing) => {
            if normalize_schema_sql(&existing) != normalize_schema_sql(CREATE_CHECKPOINTS_TABLE_SQL)
            {
                return Err(schema_mismatch(
                    "existing checkpoints table does not match the current schema; create a new database",
                ));
            }
            let existing_index = schema_sql(connection, "index", "checkpoints_status_idx")?
                .ok_or_else(|| {
                    schema_mismatch(
                        "existing checkpoints index does not match the current schema; create a new database",
                    )
                })?;
            if normalize_schema_sql(&existing_index)
                != normalize_schema_sql(CREATE_CHECKPOINTS_STATUS_INDEX_SQL)
            {
                return Err(schema_mismatch(
                    "existing checkpoints index does not match the current schema; create a new database",
                ));
            }
        }
    }
    Ok(())
}

fn schema_sql(
    connection: &Connection,
    object_type: &str,
    name: &str,
) -> CheckpointResult<Option<String>> {
    connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = ?1 AND name = ?2",
            params![object_type, name],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map(Option::flatten)
        .map_err(sqlite_error)
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
                    cycle_index, status, messages, cycles, shared_state, budget_usage,
                    event_cursor, event_outbox, extension_state, model_call_journal,
                    tool_journal, revision, claim_token, claimed_cycle,
                    lease_expires_at_ms, terminal_result, terminal_acknowledged
                ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                    ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26
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
        self.lock()?
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
}

impl SqliteCheckpointStore {
    fn replace_claimed(
        &self,
        checkpoint: Checkpoint,
        claim_token: &str,
        expected_revision: u64,
        kind: ReplaceKind,
    ) -> CheckpointResult<bool> {
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite_error)?;
        let Some(current) = load_row_transaction(&transaction, &checkpoint.checkpoint_key)? else {
            transaction.commit().map_err(sqlite_error)?;
            return Ok(false);
        };
        let updated = match kind {
            ReplaceKind::Progress => {
                prepare_progress(&current, checkpoint, claim_token, expected_revision)?
            }
            ReplaceKind::Suspend => {
                prepare_suspend(&current, checkpoint, claim_token, expected_revision)?
            }
            ReplaceKind::Commit => {
                prepare_commit(&current, checkpoint, claim_token, expected_revision)?
            }
            ReplaceKind::FinalizeClaimed => {
                prepare_finalize_claimed(&current, checkpoint, claim_token, expected_revision)?
            }
        };
        let Some(updated) = updated else {
            transaction.commit().map_err(sqlite_error)?;
            return Ok(false);
        };
        let values = SqlValues::from_checkpoint(&updated)?;
        let changed = update_row(
            &transaction,
            &values,
            Some(expected_revision),
            Some(claim_token),
        )?;
        transaction.commit().map_err(sqlite_error)?;
        Ok(changed)
    }
}

#[derive(Clone, Copy)]
enum ReplaceKind {
    Progress,
    Suspend,
    Commit,
    FinalizeClaimed,
}

struct SqlValues {
    checkpoint_key: String,
    schema_version: String,
    run_definition_schema: String,
    run_definition: String,
    task_id: String,
    root_run_id: String,
    trace_id: String,
    run_definition_digest: String,
    resume_attempt: i64,
    cycle_index: i64,
    status: String,
    messages: String,
    cycles: String,
    shared_state: String,
    budget_usage: Option<String>,
    event_cursor: Option<String>,
    event_outbox: String,
    extension_state: String,
    model_call_journal: String,
    tool_journal: String,
    revision: i64,
    claim_token: Option<String>,
    claimed_cycle: Option<i64>,
    lease_expires_at_ms: Option<i64>,
    terminal_result: Option<String>,
    terminal_acknowledged: i64,
}

impl SqlValues {
    fn from_checkpoint(checkpoint: &Checkpoint) -> CheckpointResult<Self> {
        let value = checkpoint_to_value(checkpoint, MAX_EXTENSION_STATE_BYTES)?;
        let object = value.as_object().expect("codec emits an object");
        Ok(Self {
            checkpoint_key: string_field(object, "checkpoint_key")?,
            schema_version: string_field(object, "schema_version")?,
            run_definition_schema: string_field(object, "run_definition_schema")?,
            run_definition: json_field(object, "run_definition")?,
            task_id: string_field(object, "task_id")?,
            root_run_id: string_field(object, "root_run_id")?,
            trace_id: string_field(object, "trace_id")?,
            run_definition_digest: string_field(object, "run_definition_digest")?,
            resume_attempt: to_i64(checkpoint.resume_attempt, "resume_attempt")?,
            cycle_index: to_i64(checkpoint.cycle_index, "cycle_index")?,
            status: string_field(object, "status")?,
            messages: json_field(object, "messages")?,
            cycles: json_field(object, "cycles")?,
            shared_state: json_field(object, "shared_state")?,
            budget_usage: nullable_json_field(object, "budget_usage")?,
            event_cursor: nullable_json_field(object, "event_cursor")?,
            event_outbox: json_field(object, "event_outbox")?,
            extension_state: json_field(object, "extension_state")?,
            model_call_journal: json_field(object, "model_call_journal")?,
            tool_journal: json_field(object, "tool_journal")?,
            revision: to_i64(checkpoint.revision, "revision")?,
            claim_token: checkpoint.claim_token.clone(),
            claimed_cycle: checkpoint
                .claimed_cycle
                .map(|value| to_i64(value, "claimed_cycle"))
                .transpose()?,
            lease_expires_at_ms: checkpoint
                .lease_expires_at_ms
                .map(|value| to_i64(value, "lease_expires_at_ms"))
                .transpose()?,
            terminal_result: nullable_json_field(object, "terminal_result")?,
            terminal_acknowledged: i64::from(checkpoint.terminal_acknowledged),
        })
    }

    fn params(&self) -> [&(dyn rusqlite::ToSql + Sync); 26] {
        [
            &self.checkpoint_key,
            &self.schema_version,
            &self.run_definition_schema,
            &self.run_definition,
            &self.task_id,
            &self.root_run_id,
            &self.trace_id,
            &self.run_definition_digest,
            &self.resume_attempt,
            &self.cycle_index,
            &self.status,
            &self.messages,
            &self.cycles,
            &self.shared_state,
            &self.budget_usage,
            &self.event_cursor,
            &self.event_outbox,
            &self.extension_state,
            &self.model_call_journal,
            &self.tool_journal,
            &self.revision,
            &self.claim_token,
            &self.claimed_cycle,
            &self.lease_expires_at_ms,
            &self.terminal_result,
            &self.terminal_acknowledged,
        ]
    }
}

fn update_row(
    transaction: &Transaction<'_>,
    values: &SqlValues,
    expected_revision: Option<u64>,
    claim_token: Option<&str>,
) -> CheckpointResult<bool> {
    let Some(expected_revision) = expected_revision else {
        return Err(CheckpointError::new(
            "checkpoint_revision_conflict",
            "an expected revision is required for an update",
        ));
    };
    let changed = transaction
        .execute(
            r#"
            UPDATE checkpoints SET
                schema_version = ?1, run_definition_schema = ?2, run_definition = ?3,
                task_id = ?4, root_run_id = ?5, trace_id = ?6, run_definition_digest = ?7,
                resume_attempt = ?8, cycle_index = ?9, status = ?10, messages = ?11,
                cycles = ?12, shared_state = ?13, budget_usage = ?14, event_cursor = ?15,
                event_outbox = ?16, extension_state = ?17, model_call_journal = ?18,
                tool_journal = ?19, revision = ?20,
                claim_token = ?21, claimed_cycle = ?22, lease_expires_at_ms = ?23,
                terminal_result = ?24, terminal_acknowledged = ?25
            WHERE checkpoint_key = ?26 AND revision = ?27
              AND (?28 IS NULL OR claim_token = ?28)
            "#,
            params![
                values.schema_version,
                values.run_definition_schema,
                values.run_definition,
                values.task_id,
                values.root_run_id,
                values.trace_id,
                values.run_definition_digest,
                values.resume_attempt,
                values.cycle_index,
                values.status,
                values.messages,
                values.cycles,
                values.shared_state,
                values.budget_usage,
                values.event_cursor,
                values.event_outbox,
                values.extension_state,
                values.model_call_journal,
                values.tool_journal,
                values.revision,
                values.claim_token,
                values.claimed_cycle,
                values.lease_expires_at_ms,
                values.terminal_result,
                values.terminal_acknowledged,
                values.checkpoint_key,
                to_i64(expected_revision, "revision")?,
                claim_token,
            ],
        )
        .map_err(sqlite_error)?;
    Ok(changed == 1)
}

fn load_row(connection: &Connection, checkpoint_key: &str) -> CheckpointResult<Option<Checkpoint>> {
    let mut statement = connection
        .prepare(
            r#"
            SELECT checkpoint_key, schema_version, run_definition_schema, run_definition,
                   task_id, root_run_id, trace_id, run_definition_digest, resume_attempt,
                   cycle_index, status, messages, cycles, shared_state, budget_usage,
                   event_cursor, event_outbox, extension_state, model_call_journal,
                   tool_journal, revision, claim_token, claimed_cycle,
                   lease_expires_at_ms, terminal_result, terminal_acknowledged
            FROM checkpoints WHERE checkpoint_key = ?1
            "#,
        )
        .map_err(sqlite_error)?;
    statement
        .query_row(params![checkpoint_key], row_to_checkpoint)
        .optional()
        .map_err(sqlite_error)?
        .transpose()
}

fn load_row_transaction(
    transaction: &Transaction<'_>,
    checkpoint_key: &str,
) -> CheckpointResult<Option<Checkpoint>> {
    let mut statement = transaction
        .prepare(
            r#"
            SELECT checkpoint_key, schema_version, run_definition_schema, run_definition,
                   task_id, root_run_id, trace_id, run_definition_digest, resume_attempt,
                   cycle_index, status, messages, cycles, shared_state, budget_usage,
                   event_cursor, event_outbox, extension_state, model_call_journal,
                   tool_journal, revision, claim_token, claimed_cycle,
                   lease_expires_at_ms, terminal_result, terminal_acknowledged
            FROM checkpoints WHERE checkpoint_key = ?1
            "#,
        )
        .map_err(sqlite_error)?;
    statement
        .query_row(params![checkpoint_key], row_to_checkpoint)
        .optional()
        .map_err(sqlite_error)?
        .transpose()
}

fn row_to_checkpoint(row: &rusqlite::Row<'_>) -> rusqlite::Result<CheckpointResult<Checkpoint>> {
    let checkpoint_key: String = row.get(0)?;
    let schema_version: String = row.get(1)?;
    let run_definition_schema: String = row.get(2)?;
    let run_definition: String = row.get(3)?;
    let task_id: String = row.get(4)?;
    let root_run_id: String = row.get(5)?;
    let trace_id: String = row.get(6)?;
    let run_definition_digest: String = row.get(7)?;
    let resume_attempt: i64 = row.get(8)?;
    let cycle_index: i64 = row.get(9)?;
    let status: String = row.get(10)?;
    let messages: String = row.get(11)?;
    let cycles: String = row.get(12)?;
    let shared_state: String = row.get(13)?;
    let budget_usage: Option<String> = row.get(14)?;
    let event_cursor: Option<String> = row.get(15)?;
    let event_outbox: String = row.get(16)?;
    let extension_state: String = row.get(17)?;
    let model_call_journal: String = row.get(18)?;
    let tool_journal: String = row.get(19)?;
    let revision: i64 = row.get(20)?;
    let claim_token: Option<String> = row.get(21)?;
    let claimed_cycle: Option<i64> = row.get(22)?;
    let lease_expires_at_ms: Option<i64> = row.get(23)?;
    let terminal_result: Option<String> = row.get(24)?;
    let terminal_acknowledged: i64 = row.get(25)?;

    let result = (|| {
        let mut object = Map::new();
        object.insert("schema_version".to_string(), Value::String(schema_version));
        object.insert(
            "run_definition_schema".to_string(),
            Value::String(run_definition_schema),
        );
        object.insert("run_definition".to_string(), parse_value(&run_definition)?);
        object.insert("checkpoint_key".to_string(), Value::String(checkpoint_key));
        object.insert("task_id".to_string(), Value::String(task_id));
        object.insert("root_run_id".to_string(), Value::String(root_run_id));
        object.insert("trace_id".to_string(), Value::String(trace_id));
        object.insert(
            "run_definition_digest".to_string(),
            Value::String(run_definition_digest),
        );
        object.insert(
            "resume_attempt".to_string(),
            Value::from(to_u64(resume_attempt)?),
        );
        object.insert("cycle_index".to_string(), Value::from(to_u64(cycle_index)?));
        object.insert("status".to_string(), Value::String(status));
        object.insert("messages".to_string(), parse_value(&messages)?);
        object.insert("cycles".to_string(), parse_value(&cycles)?);
        object.insert("shared_state".to_string(), parse_value(&shared_state)?);
        object.insert(
            "budget_usage".to_string(),
            optional_value(budget_usage.as_deref())?,
        );
        object.insert(
            "event_cursor".to_string(),
            optional_value(event_cursor.as_deref())?,
        );
        object.insert("event_outbox".to_string(), parse_value(&event_outbox)?);
        object.insert(
            "extension_state".to_string(),
            parse_value(&extension_state)?,
        );
        object.insert(
            "model_call_journal".to_string(),
            parse_value(&model_call_journal)?,
        );
        object.insert("tool_journal".to_string(), parse_value(&tool_journal)?);
        object.insert("revision".to_string(), Value::from(to_u64(revision)?));
        object.insert(
            "claim_token".to_string(),
            claim_token.map_or(Value::Null, Value::String),
        );
        object.insert(
            "claimed_cycle".to_string(),
            claimed_cycle.map_or(Ok(Value::Null), |value| to_u64(value).map(Value::from))?,
        );
        object.insert(
            "lease_expires_at_ms".to_string(),
            lease_expires_at_ms.map_or(Ok(Value::Null), |value| to_u64(value).map(Value::from))?,
        );
        object.insert(
            "terminal_result".to_string(),
            optional_value(terminal_result.as_deref())?,
        );
        object.insert(
            "terminal_acknowledged".to_string(),
            Value::Bool(terminal_acknowledged != 0),
        );
        checkpoint_from_value(&Value::Object(object), MAX_EXTENSION_STATE_BYTES)
    })();
    Ok(result)
}

fn string_field(object: &Map<String, Value>, field: &str) -> CheckpointResult<String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            CheckpointError::new("checkpoint_row_invalid", format!("{field} is not a string"))
        })
}

fn json_field(object: &Map<String, Value>, field: &str) -> CheckpointResult<String> {
    serde_json::to_string(object.get(field).ok_or_else(|| {
        CheckpointError::new("checkpoint_row_invalid", format!("{field} is missing"))
    })?)
    .map_err(|error| CheckpointError::new("checkpoint_json_invalid", error.to_string()))
}

fn nullable_json_field(
    object: &Map<String, Value>,
    field: &str,
) -> CheckpointResult<Option<String>> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => serde_json::to_string(value)
            .map(Some)
            .map_err(|error| CheckpointError::new("checkpoint_json_invalid", error.to_string())),
    }
}

fn parse_value(raw: &str) -> CheckpointResult<Value> {
    serde_json::from_str(raw)
        .map_err(|error| CheckpointError::new("checkpoint_json_invalid", error.to_string()))
}

fn optional_value(raw: Option<&str>) -> CheckpointResult<Value> {
    raw.map_or(Ok(Value::Null), parse_value)
}

fn to_i64(value: u64, field: &str) -> CheckpointResult<i64> {
    i64::try_from(value).map_err(|_| {
        CheckpointError::new(
            "checkpoint_integer_invalid",
            format!("{field} does not fit SQLite INTEGER"),
        )
    })
}

fn to_u64(value: i64) -> CheckpointResult<u64> {
    u64::try_from(value).map_err(|_| {
        CheckpointError::new(
            "checkpoint_row_invalid",
            "negative SQLite integer in checkpoint",
        )
    })
}

fn sqlite_error(error: rusqlite::Error) -> CheckpointError {
    CheckpointError::new("checkpoint_store_sqlite", error.to_string())
}
