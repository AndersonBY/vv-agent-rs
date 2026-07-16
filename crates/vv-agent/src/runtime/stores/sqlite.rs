use std::io::{Error, ErrorKind, Result};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};

use crate::runtime::checkpoint_codec::{checkpoint_from_json, validate_checkpoint};
use crate::runtime::state::{
    check_claim, checkpoint_status_value, clear_claim, validate_claim, validate_renew, Checkpoint,
    LeaseOperationClock, StateStore, StateStoreSpec,
};

const SELECT_CHECKPOINT: &str = "SELECT task_id, cycle_index, status, messages, cycles, \
    shared_state, revision, claim_token, claimed_cycle, lease_expires_at_ms, terminal_result, budget_usage \
    FROM checkpoints";

#[derive(Debug)]
pub struct SqliteStateStore {
    connection: Mutex<Connection>,
    location: Option<String>,
}

impl SqliteStateStore {
    pub fn new(db_path: impl AsRef<Path>) -> Result<Self> {
        let raw_path = db_path.as_ref();
        let (open_path, location) = normalize_path(raw_path)?;
        let connection = Connection::open(&open_path).map_err(sqlite_to_io)?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(sqlite_to_io)?;
        connection
            .execute_batch(
                r#"
                PRAGMA journal_mode=WAL;
                CREATE TABLE IF NOT EXISTS checkpoints (
                    task_id TEXT PRIMARY KEY,
                    cycle_index INTEGER NOT NULL,
                    status TEXT NOT NULL,
                    messages TEXT NOT NULL,
                    cycles TEXT NOT NULL,
                    shared_state TEXT NOT NULL,
                    revision INTEGER NOT NULL DEFAULT 0,
                    claim_token TEXT,
                    claimed_cycle INTEGER,
                    lease_expires_at_ms INTEGER,
                    terminal_result TEXT,
                    budget_usage TEXT
                );
                "#,
            )
            .map_err(sqlite_to_io)?;
        migrate_control_columns(&connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
            location,
        })
    }

    pub fn close(self) -> Result<()> {
        let connection = self
            .connection
            .into_inner()
            .map_err(|_| Error::other("sqlite state store lock is poisoned"))?;
        connection.close().map_err(|(_, error)| sqlite_to_io(error))
    }
}

impl StateStore for SqliteStateStore {
    fn create_checkpoint(&self, checkpoint: Checkpoint) -> Result<bool> {
        let values = checkpoint_values(&checkpoint)?;
        let changed = self
            .connection
            .lock()
            .map_err(|_| poisoned())?
            .execute(
                r#"
                INSERT OR IGNORE INTO checkpoints
                    (task_id, cycle_index, status, messages, cycles, shared_state,
                     revision, claim_token, claimed_cycle, lease_expires_at_ms, terminal_result,
                     budget_usage)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                "#,
                params![
                    values.task_id,
                    values.cycle_index,
                    values.status,
                    values.messages,
                    values.cycles,
                    values.shared_state,
                    values.revision,
                    values.claim_token,
                    values.claimed_cycle,
                    values.lease_expires_at_ms,
                    values.terminal_result,
                    values.budget_usage,
                ],
            )
            .map_err(sqlite_to_io)?;
        Ok(changed == 1)
    }

    fn save_checkpoint(&self, checkpoint: Checkpoint) -> Result<()> {
        let values = checkpoint_values(&checkpoint)?;
        self.connection
            .lock()
            .map_err(|_| poisoned())?
            .execute(
                r#"
                INSERT OR REPLACE INTO checkpoints
                    (task_id, cycle_index, status, messages, cycles, shared_state,
                     revision, claim_token, claimed_cycle, lease_expires_at_ms, terminal_result,
                     budget_usage)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                "#,
                params![
                    values.task_id,
                    values.cycle_index,
                    values.status,
                    values.messages,
                    values.cycles,
                    values.shared_state,
                    values.revision,
                    values.claim_token,
                    values.claimed_cycle,
                    values.lease_expires_at_ms,
                    values.terminal_result,
                    values.budget_usage,
                ],
            )
            .map_err(sqlite_to_io)?;
        Ok(())
    }

    fn load_checkpoint(&self, task_id: &str) -> Result<Option<Checkpoint>> {
        let connection = self.connection.lock().map_err(|_| poisoned())?;
        load_checkpoint_row(&connection, task_id)
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
        let mut connection = self.connection.lock().map_err(|_| poisoned())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite_to_io)?;
        let Some(mut checkpoint) = load_checkpoint_row(&transaction, task_id)? else {
            transaction.commit().map_err(sqlite_to_io)?;
            return Ok(None);
        };
        check_claim(&checkpoint, cycle_index, now_ms)?;
        let next_revision = checkpoint
            .revision
            .checked_add(1)
            .ok_or_else(|| Error::new(ErrorKind::InvalidData, "checkpoint revision overflow"))?;
        let changed = transaction
            .execute(
                r#"
                UPDATE checkpoints
                SET revision = ?1, claim_token = ?2, claimed_cycle = ?3, lease_expires_at_ms = ?4
                WHERE task_id = ?5 AND revision = ?6
                  AND (claim_token IS NULL OR lease_expires_at_ms <= ?7)
                "#,
                params![
                    to_sql_u64(next_revision, "revision")?,
                    claim_token,
                    cycle_index,
                    to_sql_u64(lease_expires_at_ms, "lease_expires_at_ms")?,
                    task_id,
                    to_sql_u64(checkpoint.revision, "revision")?,
                    to_sql_u64(now_ms, "now_ms")?,
                ],
            )
            .map_err(sqlite_to_io)?;
        if changed != 1 {
            return Err(Error::new(
                ErrorKind::AlreadyExists,
                format!("checkpoint cycle {cycle_index} for task {task_id} is already claimed"),
            ));
        }
        transaction.commit().map_err(sqlite_to_io)?;
        checkpoint.revision = next_revision;
        checkpoint.claim_token = Some(claim_token.to_string());
        checkpoint.claimed_cycle = Some(cycle_index);
        checkpoint.lease_expires_at_ms = Some(lease_expires_at_ms);
        Ok(Some(checkpoint))
    }

    fn commit_checkpoint(
        &self,
        mut checkpoint: Checkpoint,
        claim_token: &str,
        expected_revision: u64,
    ) -> Result<bool> {
        let claimed_cycle = checkpoint.cycle_index;
        let next_revision = expected_revision
            .checked_add(1)
            .ok_or_else(|| Error::new(ErrorKind::InvalidData, "checkpoint revision overflow"))?;
        checkpoint.revision = next_revision;
        clear_claim(&mut checkpoint);
        let values = checkpoint_values(&checkpoint)?;
        let changed = self
            .connection
            .lock()
            .map_err(|_| poisoned())?
            .execute(
                r#"
                UPDATE checkpoints
                SET cycle_index = ?1, status = ?2, messages = ?3, cycles = ?4,
                    shared_state = ?5, revision = ?6, claim_token = NULL,
                    claimed_cycle = NULL, lease_expires_at_ms = NULL, terminal_result = ?7,
                    budget_usage = ?8
                WHERE task_id = ?9 AND revision = ?10 AND claim_token = ?11 AND claimed_cycle = ?12
                "#,
                params![
                    values.cycle_index,
                    values.status,
                    values.messages,
                    values.cycles,
                    values.shared_state,
                    values.revision,
                    values.terminal_result,
                    values.budget_usage,
                    values.task_id,
                    to_sql_u64(expected_revision, "revision")?,
                    claim_token,
                    claimed_cycle,
                ],
            )
            .map_err(sqlite_to_io)?;
        Ok(changed == 1)
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
        let clock = LeaseOperationClock::new(now_ms);
        let mut connection = self.connection.lock().map_err(|_| poisoned())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite_to_io)?;
        let current_now_ms = clock.now_ms();
        if lease_expires_at_ms <= current_now_ms {
            transaction.commit().map_err(sqlite_to_io)?;
            return Ok(false);
        }
        let changed = transaction
            .execute(
                r#"
                UPDATE checkpoints
                SET lease_expires_at_ms = ?1
                WHERE task_id = ?2 AND revision = ?3 AND claim_token = ?4
                  AND lease_expires_at_ms > ?5
                "#,
                params![
                    to_sql_u64(lease_expires_at_ms, "lease_expires_at_ms")?,
                    task_id,
                    to_sql_u64(expected_revision, "revision")?,
                    claim_token,
                    to_sql_u64(current_now_ms, "now_ms")?,
                ],
            )
            .map_err(sqlite_to_io)?;
        transaction.commit().map_err(sqlite_to_io)?;
        Ok(changed == 1)
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
        checkpoint.revision = expected_revision
            .checked_add(1)
            .ok_or_else(|| Error::new(ErrorKind::InvalidData, "checkpoint revision overflow"))?;
        clear_claim(&mut checkpoint);
        let values = checkpoint_values(&checkpoint)?;
        let changed = self
            .connection
            .lock()
            .map_err(|_| poisoned())?
            .execute(
                r#"
                UPDATE checkpoints
                SET cycle_index = ?1, status = ?2, messages = ?3, cycles = ?4,
                    shared_state = ?5, revision = ?6, claim_token = NULL,
                    claimed_cycle = NULL, lease_expires_at_ms = NULL, terminal_result = ?7,
                    budget_usage = ?8
                WHERE task_id = ?9 AND revision = ?10 AND claim_token IS NULL
                  AND terminal_result IS NULL
                "#,
                params![
                    values.cycle_index,
                    values.status,
                    values.messages,
                    values.cycles,
                    values.shared_state,
                    values.revision,
                    values.terminal_result,
                    values.budget_usage,
                    values.task_id,
                    to_sql_u64(expected_revision, "revision")?,
                ],
            )
            .map_err(sqlite_to_io)?;
        Ok(changed == 1)
    }

    fn delete_checkpoint(&self, task_id: &str) -> Result<()> {
        self.connection
            .lock()
            .map_err(|_| poisoned())?
            .execute(
                "DELETE FROM checkpoints WHERE task_id = ?1",
                params![task_id],
            )
            .map_err(sqlite_to_io)?;
        Ok(())
    }

    fn acknowledge_terminal(&self, task_id: &str, expected_revision: u64) -> Result<bool> {
        let changed = self
            .connection
            .lock()
            .map_err(|_| poisoned())?
            .execute(
                "DELETE FROM checkpoints WHERE task_id = ?1 AND revision = ?2 AND terminal_result IS NOT NULL",
                params![task_id, to_sql_u64(expected_revision, "revision")?],
            )
            .map_err(sqlite_to_io)?;
        Ok(changed == 1)
    }

    fn list_checkpoints(&self) -> Result<Vec<String>> {
        let connection = self.connection.lock().map_err(|_| poisoned())?;
        let mut statement = connection
            .prepare("SELECT task_id FROM checkpoints ORDER BY task_id")
            .map_err(sqlite_to_io)?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(sqlite_to_io)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_to_io)
    }

    fn state_store_spec(&self) -> Option<StateStoreSpec> {
        self.location
            .as_ref()
            .and_then(|location| StateStoreSpec::sqlite(location).ok())
    }
}

#[derive(Debug)]
struct CheckpointValues {
    task_id: String,
    cycle_index: u32,
    status: String,
    messages: String,
    cycles: String,
    shared_state: String,
    revision: i64,
    claim_token: Option<String>,
    claimed_cycle: Option<u32>,
    lease_expires_at_ms: Option<i64>,
    terminal_result: Option<String>,
    budget_usage: Option<String>,
}

fn checkpoint_values(checkpoint: &Checkpoint) -> Result<CheckpointValues> {
    validate_checkpoint(checkpoint)?;
    Ok(CheckpointValues {
        task_id: checkpoint.task_id.clone(),
        cycle_index: checkpoint.cycle_index,
        status: checkpoint_status_value(checkpoint.status).to_string(),
        messages: serde_json::to_string(
            &checkpoint
                .messages
                .iter()
                .map(crate::types::Message::to_dict)
                .collect::<Vec<_>>(),
        )
        .map_err(json_to_io)?,
        cycles: serde_json::to_string(
            &checkpoint
                .cycles
                .iter()
                .map(crate::types::CycleRecord::to_dict)
                .collect::<Vec<_>>(),
        )
        .map_err(json_to_io)?,
        shared_state: serde_json::to_string(&checkpoint.shared_state).map_err(json_to_io)?,
        revision: to_sql_u64(checkpoint.revision, "revision")?,
        claim_token: checkpoint.claim_token.clone(),
        claimed_cycle: checkpoint.claimed_cycle,
        lease_expires_at_ms: checkpoint
            .lease_expires_at_ms
            .map(|value| to_sql_u64(value, "lease_expires_at_ms"))
            .transpose()?,
        terminal_result: checkpoint
            .terminal_result
            .as_ref()
            .map(|result| serde_json::to_string(&result.to_dict()).map_err(json_to_io))
            .transpose()?,
        budget_usage: checkpoint
            .budget_usage
            .as_ref()
            .map(|usage| serde_json::to_string(usage).map_err(json_to_io))
            .transpose()?,
    })
}

type CheckpointRow = (
    String,
    u32,
    String,
    String,
    String,
    String,
    i64,
    Option<String>,
    Option<u32>,
    Option<i64>,
    Option<String>,
    Option<String>,
);

fn load_checkpoint_row(connection: &Connection, task_id: &str) -> Result<Option<Checkpoint>> {
    let row = connection
        .query_row(
            &format!("{SELECT_CHECKPOINT} WHERE task_id = ?1"),
            params![task_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                ))
            },
        )
        .optional()
        .map_err(sqlite_to_io)?;
    row.map(checkpoint_from_row).transpose()
}

fn checkpoint_from_row(row: CheckpointRow) -> Result<Checkpoint> {
    let revision = u64::try_from(row.6).map_err(|_| {
        Error::new(
            ErrorKind::InvalidData,
            "checkpoint revision must be non-negative",
        )
    })?;
    let lease_expires_at_ms = row
        .9
        .map(|value| {
            u64::try_from(value).map_err(|_| {
                Error::new(
                    ErrorKind::InvalidData,
                    "checkpoint lease_expires_at_ms must be non-negative",
                )
            })
        })
        .transpose()?;
    let payload = json!({
        "task_id": row.0,
        "cycle_index": row.1,
        "status": row.2,
        "messages": parse_json(&row.3, "messages")?,
        "cycles": parse_json(&row.4, "cycles")?,
        "shared_state": parse_json(&row.5, "shared_state")?,
        "revision": revision,
        "claim_token": row.7,
        "claimed_cycle": row.8,
        "lease_expires_at_ms": lease_expires_at_ms,
        "terminal_result": row.10.as_deref().map(|value| parse_json(value, "terminal_result")).transpose()?,
        "budget_usage": row.11.as_deref().map(|value| parse_json(value, "budget_usage")).transpose()?,
    });
    checkpoint_from_json(&payload.to_string())
}

fn normalize_path(path: &Path) -> Result<(PathBuf, Option<String>)> {
    if path == Path::new(":memory:") {
        return Ok((PathBuf::from(":memory:"), None));
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let location = absolute.to_string_lossy().to_string();
    Ok((absolute, Some(location)))
}

fn migrate_control_columns(connection: &Connection) -> Result<()> {
    let mut statement = connection
        .prepare("PRAGMA table_info(checkpoints)")
        .map_err(sqlite_to_io)?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(sqlite_to_io)?
        .collect::<rusqlite::Result<std::collections::BTreeSet<_>>>()
        .map_err(sqlite_to_io)?;
    drop(statement);
    for (name, declaration) in [
        ("revision", "INTEGER NOT NULL DEFAULT 0"),
        ("claim_token", "TEXT"),
        ("claimed_cycle", "INTEGER"),
        ("lease_expires_at_ms", "INTEGER"),
        ("terminal_result", "TEXT"),
        ("budget_usage", "TEXT"),
    ] {
        if !columns.contains(name) {
            connection
                .execute(
                    &format!("ALTER TABLE checkpoints ADD COLUMN {name} {declaration}"),
                    [],
                )
                .map_err(sqlite_to_io)?;
        }
    }
    Ok(())
}

fn parse_json(raw: &str, field: &str) -> Result<Value> {
    serde_json::from_str(raw).map_err(|error| {
        Error::new(
            ErrorKind::InvalidData,
            format!("invalid checkpoint {field} JSON: {error}"),
        )
    })
}

fn to_sql_u64(value: u64, field: &str) -> Result<i64> {
    i64::try_from(value).map_err(|_| {
        Error::new(
            ErrorKind::InvalidData,
            format!("checkpoint {field} exceeds SQLite INTEGER range"),
        )
    })
}

fn poisoned() -> Error {
    Error::other("sqlite state store lock is poisoned")
}

fn json_to_io(error: serde_json::Error) -> Error {
    Error::new(ErrorKind::InvalidData, error)
}

fn sqlite_to_io(error: rusqlite::Error) -> Error {
    Error::other(error.to_string())
}
