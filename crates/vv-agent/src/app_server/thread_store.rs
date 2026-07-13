use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

use crate::app_server::protocol::{
    AppItem, AppThread, AppTurn, ThreadStartParams, ThreadStatus, TurnStatus, UserInput,
};

#[derive(Clone)]
pub struct SqliteThreadStore {
    connection: Arc<Mutex<Connection>>,
    next_thread_id: Arc<AtomicU64>,
    next_turn_id: Arc<AtomicU64>,
}

impl SqliteThreadStore {
    pub fn in_memory() -> Result<Self, ThreadStoreError> {
        let connection = Connection::open_in_memory().map_err(ThreadStoreError::sql)?;
        Self::from_connection(connection)
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, ThreadStoreError> {
        let connection = Connection::open(path).map_err(ThreadStoreError::sql)?;
        Self::from_connection(connection)
    }

    fn from_connection(connection: Connection) -> Result<Self, ThreadStoreError> {
        let store = Self {
            connection: Arc::new(Mutex::new(connection)),
            next_thread_id: Arc::new(AtomicU64::new(1)),
            next_turn_id: Arc::new(AtomicU64::new(1)),
        };
        store.migrate()?;
        store.recover_interrupted_threads()?;
        store.seed_sequences()?;
        Ok(store)
    }

    pub fn create_thread(&self, params: ThreadStartParams) -> Result<AppThread, ThreadStoreError> {
        let sequence = self.next_thread_id.fetch_add(1, Ordering::Relaxed);
        let now = timestamp_seconds();
        let thread = AppThread {
            thread_id: format!("thread_{sequence}"),
            agent_key: params.agent_key,
            cwd: params.cwd,
            created_at: now,
            updated_at: now,
            archived_at: None,
            status: ThreadStatus::Idle,
            metadata: params.metadata,
        };
        self.insert_thread(&thread)?;
        Ok(thread)
    }

    pub fn get_thread(&self, thread_id: &str) -> Result<Option<AppThread>, ThreadStoreError> {
        let connection = self.connection.lock().map_err(ThreadStoreError::poisoned)?;
        connection
            .query_row(
                "SELECT thread_id, agent_key, cwd, created_at, updated_at, archived_at, status, metadata_json
                 FROM app_server_threads WHERE thread_id = ?1",
                params![thread_id],
                row_to_thread,
            )
            .optional()
            .map_err(ThreadStoreError::sql)
    }

    pub fn list_threads(&self, include_archived: bool) -> Result<Vec<AppThread>, ThreadStoreError> {
        let connection = self.connection.lock().map_err(ThreadStoreError::poisoned)?;
        let sql = if include_archived {
            "SELECT thread_id, agent_key, cwd, created_at, updated_at, archived_at, status, metadata_json
             FROM app_server_threads ORDER BY rowid ASC"
        } else {
            "SELECT thread_id, agent_key, cwd, created_at, updated_at, archived_at, status, metadata_json
             FROM app_server_threads WHERE archived_at IS NULL ORDER BY rowid ASC"
        };
        let mut statement = connection.prepare(sql).map_err(ThreadStoreError::sql)?;
        let rows = statement
            .query_map([], row_to_thread)
            .map_err(ThreadStoreError::sql)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(ThreadStoreError::sql)
    }

    pub fn archive_thread(&self, thread_id: &str) -> Result<(), ThreadStoreError> {
        let now = timestamp_seconds();
        let connection = self.connection.lock().map_err(ThreadStoreError::poisoned)?;
        let changed = connection
            .execute(
                "UPDATE app_server_threads
                 SET archived_at = ?2, status = 'archived', updated_at = ?2
                 WHERE thread_id = ?1",
                params![thread_id, now],
            )
            .map_err(ThreadStoreError::sql)?;
        if changed == 0 {
            return Err(ThreadStoreError::not_found("thread", thread_id));
        }
        Ok(())
    }

    pub fn set_active_turn(
        &self,
        thread_id: &str,
        active_turn_id: Option<&str>,
        status: ThreadStatus,
    ) -> Result<(), ThreadStoreError> {
        let now = timestamp_seconds();
        let connection = self.connection.lock().map_err(ThreadStoreError::poisoned)?;
        let changed = connection
            .execute(
                "UPDATE app_server_threads
                 SET active_turn_id = ?2, status = ?3, updated_at = ?4
                 WHERE thread_id = ?1",
                params![thread_id, active_turn_id, thread_status_to_str(status), now],
            )
            .map_err(ThreadStoreError::sql)?;
        if changed == 0 {
            return Err(ThreadStoreError::not_found("thread", thread_id));
        }
        Ok(())
    }

    pub fn create_turn(
        &self,
        thread_id: &str,
        input: Vec<UserInput>,
    ) -> Result<AppTurn, ThreadStoreError> {
        let sequence = self.next_turn_id.fetch_add(1, Ordering::Relaxed);
        let turn = AppTurn {
            turn_id: format!("turn_{sequence}"),
            thread_id: thread_id.to_string(),
            run_id: None,
            status: TurnStatus::Running,
            started_at: timestamp_seconds(),
            completed_at: None,
            input,
            result: BTreeMap::new(),
        };
        let input_json = serde_json::to_string(&turn.input).map_err(ThreadStoreError::json)?;
        let result_json = serde_json::to_string(&turn.result).map_err(ThreadStoreError::json)?;
        let connection = self.connection.lock().map_err(ThreadStoreError::poisoned)?;
        connection
            .execute(
                "INSERT INTO app_server_turns
                 (turn_id, thread_id, run_id, status, started_at, completed_at, input_json, result_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    turn.turn_id,
                    turn.thread_id,
                    turn.run_id,
                    turn_status_to_str(turn.status),
                    turn.started_at,
                    turn.completed_at,
                    input_json,
                    result_json,
                ],
            )
            .map_err(ThreadStoreError::sql)?;
        Ok(turn)
    }

    pub fn update_turn(
        &self,
        turn_id: &str,
        status: TurnStatus,
        run_id: Option<&str>,
        result: &BTreeMap<String, Value>,
    ) -> Result<AppTurn, ThreadStoreError> {
        let completed_at = timestamp_seconds();
        let result_json = serde_json::to_string(result).map_err(ThreadStoreError::json)?;
        let connection = self.connection.lock().map_err(ThreadStoreError::poisoned)?;
        let changed = connection
            .execute(
                "UPDATE app_server_turns
                 SET status = ?2, run_id = COALESCE(?3, run_id), completed_at = ?4, result_json = ?5
                 WHERE turn_id = ?1",
                params![
                    turn_id,
                    turn_status_to_str(status),
                    run_id,
                    completed_at,
                    result_json,
                ],
            )
            .map_err(ThreadStoreError::sql)?;
        if changed == 0 {
            return Err(ThreadStoreError::not_found("turn", turn_id));
        }
        query_turn(&connection, turn_id)?
            .ok_or_else(|| ThreadStoreError::not_found("turn", turn_id))
    }

    pub fn list_turns(&self, thread_id: &str) -> Result<Vec<AppTurn>, ThreadStoreError> {
        let connection = self.connection.lock().map_err(ThreadStoreError::poisoned)?;
        let mut statement = connection
            .prepare(
                "SELECT turn_id, thread_id, run_id, status, started_at, completed_at, input_json, result_json
                 FROM app_server_turns WHERE thread_id = ?1 ORDER BY rowid ASC",
            )
            .map_err(ThreadStoreError::sql)?;
        let rows = statement
            .query_map(params![thread_id], row_to_turn)
            .map_err(ThreadStoreError::sql)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(ThreadStoreError::sql)
    }

    pub fn append_item(
        &self,
        thread_id: &str,
        turn_id: &str,
        item: AppItem,
    ) -> Result<(), ThreadStoreError> {
        let payload_json = serde_json::to_string(&item).map_err(ThreadStoreError::json)?;
        let connection = self.connection.lock().map_err(ThreadStoreError::poisoned)?;
        let sequence: i64 = connection
            .query_row(
                "SELECT COALESCE(MAX(sequence), 0) + 1 FROM app_server_items WHERE thread_id = ?1",
                params![thread_id],
                |row| row.get(0),
            )
            .map_err(ThreadStoreError::sql)?;
        connection
            .execute(
                "INSERT INTO app_server_items
                 (item_id, thread_id, turn_id, sequence, payload_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![item.item_id, thread_id, turn_id, sequence, payload_json],
            )
            .map_err(ThreadStoreError::sql)?;
        Ok(())
    }

    pub fn replay_items(&self, thread_id: &str) -> Result<Vec<AppItem>, ThreadStoreError> {
        let connection = self.connection.lock().map_err(ThreadStoreError::poisoned)?;
        let mut statement = connection
            .prepare(
                "SELECT payload_json FROM app_server_items
                 WHERE thread_id = ?1 ORDER BY sequence ASC",
            )
            .map_err(ThreadStoreError::sql)?;
        let rows = statement
            .query_map(params![thread_id], |row| row.get::<_, String>(0))
            .map_err(ThreadStoreError::sql)?;
        let mut items = Vec::new();
        for row in rows {
            let payload_json = row.map_err(ThreadStoreError::sql)?;
            let mut item: AppItem =
                serde_json::from_str(&payload_json).map_err(ThreadStoreError::json)?;
            item.created_at = normalize_timestamp(item.created_at);
            item.updated_at = normalize_timestamp(item.updated_at);
            items.push(item);
        }
        Ok(items)
    }

    fn insert_thread(&self, thread: &AppThread) -> Result<(), ThreadStoreError> {
        let cwd = thread
            .cwd
            .as_ref()
            .map(|path| path_to_string(path.as_path()));
        let metadata_json =
            serde_json::to_string(&thread.metadata).map_err(ThreadStoreError::json)?;
        let connection = self.connection.lock().map_err(ThreadStoreError::poisoned)?;
        connection
            .execute(
                "INSERT INTO app_server_threads
                 (thread_id, agent_key, cwd, created_at, updated_at, archived_at, status, metadata_json, active_turn_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL)",
                params![
                    thread.thread_id,
                    thread.agent_key,
                    cwd,
                    thread.created_at,
                    thread.updated_at,
                    thread.archived_at,
                    thread_status_to_str(thread.status),
                    metadata_json,
                ],
            )
            .map_err(ThreadStoreError::sql)?;
        Ok(())
    }

    fn migrate(&self) -> Result<(), ThreadStoreError> {
        let connection = self.connection.lock().map_err(ThreadStoreError::poisoned)?;
        connection
            .execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS app_server_threads (
                    thread_id TEXT PRIMARY KEY,
                    agent_key TEXT NOT NULL,
                    cwd TEXT,
                    created_at REAL NOT NULL,
                    updated_at REAL NOT NULL,
                    archived_at REAL,
                    status TEXT NOT NULL,
                    metadata_json TEXT NOT NULL,
                    active_turn_id TEXT
                );

                CREATE TABLE IF NOT EXISTS app_server_turns (
                    turn_id TEXT PRIMARY KEY,
                    thread_id TEXT NOT NULL,
                    run_id TEXT,
                    status TEXT NOT NULL,
                    started_at REAL NOT NULL,
                    completed_at REAL,
                    input_json TEXT NOT NULL,
                    result_json TEXT NOT NULL
                );

                CREATE TABLE IF NOT EXISTS app_server_items (
                    item_id TEXT PRIMARY KEY,
                    thread_id TEXT NOT NULL,
                    turn_id TEXT NOT NULL,
                    sequence INTEGER NOT NULL,
                    payload_json TEXT NOT NULL
                );

                CREATE INDEX IF NOT EXISTS idx_app_server_items_thread_sequence
                    ON app_server_items(thread_id, sequence);
                CREATE INDEX IF NOT EXISTS idx_app_server_turns_thread
                    ON app_server_turns(thread_id);
                "#,
            )
            .map_err(ThreadStoreError::sql)?;
        ensure_column(
            &connection,
            "app_server_threads",
            "status",
            "TEXT NOT NULL DEFAULT 'idle'",
        )?;
        ensure_column(&connection, "app_server_threads", "active_turn_id", "TEXT")?;
        Ok(())
    }

    fn recover_interrupted_threads(&self) -> Result<(), ThreadStoreError> {
        let connection = self.connection.lock().map_err(ThreadStoreError::poisoned)?;
        connection
            .execute(
                "UPDATE app_server_threads
                 SET status = 'idle', active_turn_id = NULL
                 WHERE status = 'running'",
                [],
            )
            .map_err(ThreadStoreError::sql)?;
        Ok(())
    }

    fn seed_sequences(&self) -> Result<(), ThreadStoreError> {
        let connection = self.connection.lock().map_err(ThreadStoreError::poisoned)?;
        let thread_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM app_server_threads", [], |row| {
                row.get(0)
            })
            .map_err(ThreadStoreError::sql)?;
        let turn_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM app_server_turns", [], |row| {
                row.get(0)
            })
            .map_err(ThreadStoreError::sql)?;
        self.next_thread_id
            .store(thread_count.max(0) as u64 + 1, Ordering::Relaxed);
        self.next_turn_id
            .store(turn_count.max(0) as u64 + 1, Ordering::Relaxed);
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadStoreError {
    message: String,
}

impl ThreadStoreError {
    fn sql(error: rusqlite::Error) -> Self {
        Self {
            message: error.to_string(),
        }
    }

    fn json(error: serde_json::Error) -> Self {
        Self {
            message: error.to_string(),
        }
    }

    fn poisoned<T>(_: std::sync::PoisonError<T>) -> Self {
        Self {
            message: "thread store lock poisoned".to_string(),
        }
    }

    fn not_found(kind: &str, id: &str) -> Self {
        Self {
            message: format!("unknown {kind}: {id}"),
        }
    }
}

impl fmt::Display for ThreadStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ThreadStoreError {}

fn row_to_thread(row: &rusqlite::Row<'_>) -> rusqlite::Result<AppThread> {
    let cwd: Option<String> = row.get("cwd")?;
    let status: String = row.get("status")?;
    let created_at = read_timestamp(row, "created_at")?;
    let updated_at = read_timestamp(row, "updated_at")?;
    let archived_at = read_optional_timestamp(row, "archived_at")?;
    let metadata_json: String = row.get("metadata_json")?;
    Ok(AppThread {
        thread_id: row.get("thread_id")?,
        agent_key: row.get("agent_key")?,
        cwd: cwd.map(PathBuf::from),
        created_at,
        updated_at,
        archived_at,
        status: thread_status_from_str(&status).map_err(conversion_error)?,
        metadata: serde_json::from_str(&metadata_json).map_err(json_conversion_error)?,
    })
}

fn row_to_turn(row: &rusqlite::Row<'_>) -> rusqlite::Result<AppTurn> {
    let status: String = row.get("status")?;
    let started_at = read_timestamp(row, "started_at")?;
    let completed_at = read_optional_timestamp(row, "completed_at")?;
    let input_json: String = row.get("input_json")?;
    let result_json: String = row.get("result_json")?;
    Ok(AppTurn {
        turn_id: row.get("turn_id")?,
        thread_id: row.get("thread_id")?,
        run_id: row.get("run_id")?,
        status: turn_status_from_str(&status).map_err(conversion_error)?,
        started_at,
        completed_at,
        input: serde_json::from_str(&input_json).map_err(json_conversion_error)?,
        result: serde_json::from_str(&result_json).map_err(json_conversion_error)?,
    })
}

fn query_turn(connection: &Connection, turn_id: &str) -> Result<Option<AppTurn>, ThreadStoreError> {
    connection
        .query_row(
            "SELECT turn_id, thread_id, run_id, status, started_at, completed_at, input_json, result_json
             FROM app_server_turns WHERE turn_id = ?1",
            params![turn_id],
            row_to_turn,
        )
        .optional()
        .map_err(ThreadStoreError::sql)
}

fn thread_status_to_str(status: ThreadStatus) -> &'static str {
    match status {
        ThreadStatus::Idle => "idle",
        ThreadStatus::Running => "running",
        ThreadStatus::Archived => "archived",
        ThreadStatus::Closed => "closed",
    }
}

fn thread_status_from_str(status: &str) -> Result<ThreadStatus, String> {
    match status {
        "idle" => Ok(ThreadStatus::Idle),
        "running" => Ok(ThreadStatus::Running),
        "archived" => Ok(ThreadStatus::Archived),
        "closed" => Ok(ThreadStatus::Closed),
        other => Err(format!("unknown thread status: {other}")),
    }
}

fn turn_status_to_str(status: TurnStatus) -> &'static str {
    match status {
        TurnStatus::Queued => "queued",
        TurnStatus::Running => "running",
        TurnStatus::Completed => "completed",
        TurnStatus::Failed => "failed",
        TurnStatus::Interrupted => "interrupted",
    }
}

fn turn_status_from_str(status: &str) -> Result<TurnStatus, String> {
    match status {
        "queued" => Ok(TurnStatus::Queued),
        "running" => Ok(TurnStatus::Running),
        "completed" => Ok(TurnStatus::Completed),
        "failed" => Ok(TurnStatus::Failed),
        "interrupted" => Ok(TurnStatus::Interrupted),
        other => Err(format!("unknown turn status: {other}")),
    }
}

fn conversion_error(message: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(ThreadStoreError { message }),
    )
}

fn json_conversion_error(error: serde_json::Error) -> rusqlite::Error {
    conversion_error(error.to_string())
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn timestamp_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or_default()
}

fn read_timestamp(row: &rusqlite::Row<'_>, column: &str) -> rusqlite::Result<f64> {
    use rusqlite::types::ValueRef;

    let value = match row.get_ref(column)? {
        ValueRef::Integer(value) => value as f64,
        ValueRef::Real(value) => value,
        other => {
            return Err(rusqlite::Error::InvalidColumnType(
                0,
                column.to_string(),
                other.data_type(),
            ))
        }
    };
    Ok(normalize_timestamp(value))
}

fn read_optional_timestamp(row: &rusqlite::Row<'_>, column: &str) -> rusqlite::Result<Option<f64>> {
    use rusqlite::types::ValueRef;

    match row.get_ref(column)? {
        ValueRef::Null => Ok(None),
        ValueRef::Integer(value) => Ok(Some(normalize_timestamp(value as f64))),
        ValueRef::Real(value) => Ok(Some(normalize_timestamp(value))),
        other => Err(rusqlite::Error::InvalidColumnType(
            0,
            column.to_string(),
            other.data_type(),
        )),
    }
}

fn normalize_timestamp(value: f64) -> f64 {
    if value.abs() >= 100_000_000_000.0 {
        value / 1000.0
    } else {
        value
    }
}

fn ensure_column(
    connection: &Connection,
    table: &str,
    column: &str,
    declaration: &str,
) -> Result<(), ThreadStoreError> {
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(ThreadStoreError::sql)?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(ThreadStoreError::sql)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(ThreadStoreError::sql)?;
    drop(statement);
    if !columns.iter().any(|existing| existing == column) {
        connection
            .execute(
                &format!("ALTER TABLE {table} ADD COLUMN {column} {declaration}"),
                [],
            )
            .map_err(ThreadStoreError::sql)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_server::protocol::{AppItemKind, AppItemStatus};

    #[test]
    fn duplicate_item_ids_fail_without_replacing_or_reordering_replay() {
        let store = SqliteThreadStore::in_memory().expect("store");
        let thread = store
            .create_thread(ThreadStartParams {
                agent_key: "default".to_string(),
                cwd: None,
                metadata: Default::default(),
            })
            .expect("thread");
        let item = AppItem {
            item_id: "item_1".to_string(),
            thread_id: thread.thread_id.clone(),
            turn_id: "turn_1".to_string(),
            kind: AppItemKind::AgentMessage,
            status: AppItemStatus::Completed,
            payload: serde_json::json!({"text": "original"}),
            created_at: 1.0,
            updated_at: 1.0,
        };
        store
            .append_item(&thread.thread_id, "turn_1", item.clone())
            .expect("first append");
        let mut replacement = item;
        replacement.payload = serde_json::json!({"text": "replacement"});

        assert!(store
            .append_item(&thread.thread_id, "turn_1", replacement)
            .is_err());
        let replay = store.replay_items(&thread.thread_id).expect("replay");
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].payload, serde_json::json!({"text": "original"}));
    }

    #[test]
    fn opening_a_legacy_database_adds_thread_lifecycle_columns() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("legacy.sqlite");
        let connection = Connection::open(&path).expect("legacy connection");
        connection
            .execute_batch(
                r#"
                CREATE TABLE app_server_threads (
                    thread_id TEXT PRIMARY KEY,
                    agent_key TEXT NOT NULL,
                    cwd TEXT,
                    created_at REAL NOT NULL,
                    updated_at REAL NOT NULL,
                    archived_at REAL,
                    metadata_json TEXT NOT NULL
                );
                INSERT INTO app_server_threads (
                    thread_id, agent_key, cwd, created_at, updated_at, archived_at, metadata_json
                ) VALUES ('thread_1', 'default', NULL, 1.0, 1.0, NULL, '{}');
                "#,
            )
            .expect("legacy schema");
        drop(connection);

        let store = SqliteThreadStore::open(&path).expect("migrated store");
        let thread = store
            .get_thread("thread_1")
            .expect("read thread")
            .expect("thread exists");
        assert_eq!(thread.status, ThreadStatus::Idle);
    }
}
