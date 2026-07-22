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

const THREAD_STORE_SCHEMA_VERSION: i64 = 1;
const THREAD_STORE_TABLE_COLUMNS: &[(&str, &[&str])] = &[
    (
        "app_server_threads",
        &[
            "thread_id",
            "agent_key",
            "cwd",
            "created_at",
            "updated_at",
            "archived_at",
            "status",
            "metadata_json",
            "active_turn_id",
        ],
    ),
    (
        "app_server_turns",
        &[
            "turn_id",
            "thread_id",
            "run_id",
            "status",
            "started_at",
            "completed_at",
            "input_json",
            "result_json",
        ],
    ),
    (
        "app_server_items",
        &[
            "item_id",
            "thread_id",
            "turn_id",
            "sequence",
            "payload_json",
        ],
    ),
];

#[derive(Clone)]
pub struct SqliteThreadStore {
    connection: Arc<Mutex<Connection>>,
    next_thread_id: Arc<AtomicU64>,
    next_turn_id: Arc<AtomicU64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemAppendOutcome {
    Inserted,
    AlreadyPresent,
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
        store.initialize_schema()?;
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

    pub fn get_turn(
        &self,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<Option<AppTurn>, ThreadStoreError> {
        let connection = self.connection.lock().map_err(ThreadStoreError::poisoned)?;
        connection
            .query_row(
                "SELECT turn_id, thread_id, run_id, status, started_at, completed_at, input_json, result_json
                 FROM app_server_turns WHERE thread_id = ?1 AND turn_id = ?2",
                params![thread_id, turn_id],
                row_to_turn,
            )
            .optional()
            .map_err(ThreadStoreError::sql)
    }

    pub fn mark_turn_running(
        &self,
        thread_id: &str,
        turn_id: &str,
        run_id: &str,
    ) -> Result<AppTurn, ThreadStoreError> {
        let connection = self.connection.lock().map_err(ThreadStoreError::poisoned)?;
        let changed = connection
            .execute(
                "UPDATE app_server_turns
                 SET status = 'running', run_id = ?3, completed_at = NULL, result_json = '{}'
                 WHERE thread_id = ?1 AND turn_id = ?2",
                params![thread_id, turn_id, run_id],
            )
            .map_err(ThreadStoreError::sql)?;
        if changed == 0 {
            return Err(ThreadStoreError::not_found("turn", turn_id));
        }
        query_turn(&connection, turn_id)?
            .ok_or_else(|| ThreadStoreError::not_found("turn", turn_id))
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
    ) -> Result<ItemAppendOutcome, ThreadStoreError> {
        if item.thread_id != thread_id || item.turn_id != turn_id {
            return Err(ThreadStoreError::item_identity_conflict(&item.item_id));
        }
        let payload_json = serde_json::to_string(&item).map_err(ThreadStoreError::json)?;
        let connection = self.connection.lock().map_err(ThreadStoreError::poisoned)?;
        let sequence: i64 = connection
            .query_row(
                "SELECT COALESCE(MAX(sequence), 0) + 1 FROM app_server_items WHERE thread_id = ?1",
                params![thread_id],
                |row| row.get(0),
            )
            .map_err(ThreadStoreError::sql)?;
        let inserted = connection
            .execute(
                "INSERT OR IGNORE INTO app_server_items
                 (item_id, thread_id, turn_id, sequence, payload_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![&item.item_id, thread_id, turn_id, sequence, payload_json],
            )
            .map_err(ThreadStoreError::sql)?;
        if inserted == 1 {
            return Ok(ItemAppendOutcome::Inserted);
        }

        let existing = connection
            .query_row(
                "SELECT thread_id, turn_id, payload_json
                 FROM app_server_items WHERE item_id = ?1",
                params![&item.item_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(ThreadStoreError::sql)?;
        let Some((existing_thread_id, existing_turn_id, existing_payload)) = existing else {
            return Err(ThreadStoreError::item_identity_conflict(&item.item_id));
        };
        let existing_item = serde_json::from_str::<AppItem>(&existing_payload)
            .map_err(|_| ThreadStoreError::item_identity_conflict(&item.item_id))?;
        if existing_thread_id == thread_id && existing_turn_id == turn_id && existing_item == item {
            Ok(ItemAppendOutcome::AlreadyPresent)
        } else {
            Err(ThreadStoreError::item_identity_conflict(&item.item_id))
        }
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

    fn initialize_schema(&self) -> Result<(), ThreadStoreError> {
        let connection = self.connection.lock().map_err(ThreadStoreError::poisoned)?;
        let version = connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .map_err(ThreadStoreError::sql)?;
        let existing_tables = schema_objects(&connection, Some("table"))?;
        if existing_tables.is_empty() {
            if version != 0 {
                return Err(ThreadStoreError::schema_version(version));
            }
            connection
                .execute_batch(
                    r#"
                PRAGMA user_version = 1;

                CREATE TABLE app_server_threads (
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

                CREATE TABLE app_server_turns (
                    turn_id TEXT PRIMARY KEY,
                    thread_id TEXT NOT NULL,
                    run_id TEXT,
                    status TEXT NOT NULL,
                    started_at REAL NOT NULL,
                    completed_at REAL,
                    input_json TEXT NOT NULL,
                    result_json TEXT NOT NULL
                );

                CREATE TABLE app_server_items (
                    item_id TEXT PRIMARY KEY,
                    thread_id TEXT NOT NULL,
                    turn_id TEXT NOT NULL,
                    sequence INTEGER NOT NULL,
                    payload_json TEXT NOT NULL
                );

                CREATE INDEX idx_app_server_items_thread_sequence
                    ON app_server_items(thread_id, sequence);
                CREATE INDEX idx_app_server_turns_thread
                    ON app_server_turns(thread_id);
                "#,
                )
                .map_err(ThreadStoreError::sql)?;
        } else if version != THREAD_STORE_SCHEMA_VERSION {
            return Err(ThreadStoreError::schema_version(version));
        }
        validate_schema(&connection)?;
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

    fn item_identity_conflict(item_id: &str) -> Self {
        Self {
            message: format!("app_server_item_identity_conflict: {item_id}"),
        }
    }

    fn schema_version(actual: i64) -> Self {
        Self {
            message: format!(
                "App Server thread-store schema version {actual} does not match required version {THREAD_STORE_SCHEMA_VERSION}"
            ),
        }
    }

    fn schema(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
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

fn schema_objects(
    connection: &Connection,
    object_type: Option<&str>,
) -> Result<Vec<(String, String)>, ThreadStoreError> {
    let type_filter = object_type.map_or(String::new(), |kind| format!(" AND type = '{kind}'"));
    let mut statement = connection
        .prepare(&format!(
            "SELECT type, name FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' AND type IN ('table', 'index'){type_filter}"
        ))
        .map_err(ThreadStoreError::sql)?;
    let mut objects = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(ThreadStoreError::sql)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(ThreadStoreError::sql)?;
    objects.sort();
    Ok(objects)
}

fn validate_schema(connection: &Connection) -> Result<(), ThreadStoreError> {
    let actual_objects = schema_objects(connection, None)?;
    let mut expected_objects = THREAD_STORE_TABLE_COLUMNS
        .iter()
        .map(|(table, _)| ("table".to_string(), (*table).to_string()))
        .chain([
            (
                "index".to_string(),
                "idx_app_server_items_thread_sequence".to_string(),
            ),
            (
                "index".to_string(),
                "idx_app_server_turns_thread".to_string(),
            ),
        ])
        .collect::<Vec<_>>();
    expected_objects.sort();
    if actual_objects != expected_objects {
        return Err(ThreadStoreError::schema(format!(
            "App Server thread-store schema objects do not match the current schema: expected={expected_objects:?}, actual={actual_objects:?}"
        )));
    }

    for (table, expected_columns) in THREAD_STORE_TABLE_COLUMNS {
        let mut statement = connection
            .prepare(&format!("PRAGMA table_info({table})"))
            .map_err(ThreadStoreError::sql)?;
        let actual_columns = statement
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(ThreadStoreError::sql)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(ThreadStoreError::sql)?;
        if actual_columns
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            != expected_columns.to_vec()
        {
            return Err(ThreadStoreError::schema(format!(
                "App Server thread-store table {table} does not match the current schema: expected={expected_columns:?}, actual={actual_columns:?}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_server::protocol::{AppItemKind, AppItemStatus};

    #[test]
    fn item_append_redelivery_survives_restart_and_rejects_conflicts() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("thread-store.sqlite");
        let store = SqliteThreadStore::open(&path).expect("store");
        let thread = store
            .create_thread(ThreadStartParams {
                agent_key: "default".to_string(),
                cwd: None,
                metadata: Default::default(),
            })
            .expect("thread");
        let turn = store
            .create_turn(&thread.thread_id, Vec::new())
            .expect("turn");
        let item = AppItem {
            item_id: "item_1".to_string(),
            thread_id: thread.thread_id.clone(),
            turn_id: turn.turn_id.clone(),
            kind: AppItemKind::AgentMessage,
            status: AppItemStatus::Completed,
            payload: serde_json::json!({"text": "original"}),
            created_at: 1.0,
            updated_at: 1.0,
        };
        assert_eq!(
            store
                .append_item(&thread.thread_id, &turn.turn_id, item.clone())
                .expect("first append"),
            ItemAppendOutcome::Inserted
        );
        drop(store);

        let store = SqliteThreadStore::open(&path).expect("reopened store");
        assert_eq!(
            store
                .append_item(&thread.thread_id, &turn.turn_id, item.clone())
                .expect("same event redelivery after restart"),
            ItemAppendOutcome::AlreadyPresent
        );
        let mut replacement = item.clone();
        replacement.payload = serde_json::json!({"text": "replacement"});

        let error = store
            .append_item(&thread.thread_id, &turn.turn_id, replacement)
            .expect_err("conflicting projection");
        assert_eq!(
            error.to_string(),
            "app_server_item_identity_conflict: item_1"
        );
        let other_turn = store
            .create_turn(&thread.thread_id, Vec::new())
            .expect("other turn");
        let mut misplaced = item.clone();
        misplaced.turn_id = other_turn.turn_id.clone();
        let error = store
            .append_item(&thread.thread_id, &other_turn.turn_id, misplaced)
            .expect_err("same identity cannot move to another turn");
        assert_eq!(
            error.to_string(),
            "app_server_item_identity_conflict: item_1"
        );

        let mut next = item.clone();
        next.item_id = "item_2".to_string();
        next.payload = serde_json::json!({"text": "next"});
        assert_eq!(
            store
                .append_item(&thread.thread_id, &turn.turn_id, next)
                .expect("next append"),
            ItemAppendOutcome::Inserted
        );
        let replay = store.replay_items(&thread.thread_id).expect("replay");
        assert_eq!(replay.len(), 2);
        assert_eq!(replay[0], item);
        assert_eq!(replay[1].item_id, "item_2");
    }

    #[test]
    fn opening_an_unversioned_database_is_rejected_without_mutation() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("unversioned.sqlite");
        let connection = Connection::open(&path).expect("unversioned connection");
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
            .expect("unversioned schema");
        drop(connection);

        let error = SqliteThreadStore::open(&path)
            .err()
            .expect("unversioned schema rejected");
        assert_eq!(
            error.to_string(),
            "App Server thread-store schema version 0 does not match required version 1"
        );
        let connection = Connection::open(&path).expect("reopen unversioned connection");
        let columns = connection
            .prepare("PRAGMA table_info(app_server_threads)")
            .expect("prepare columns")
            .query_map([], |row| row.get::<_, String>(1))
            .expect("query columns")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect columns");
        assert!(!columns.iter().any(|column| column == "status"));
        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .expect("user version"),
            0
        );
    }

    #[test]
    fn opening_a_wrong_schema_version_is_rejected() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("wrong-version.sqlite");
        SqliteThreadStore::open(&path).expect("current store");
        let connection = Connection::open(&path).expect("connection");
        connection
            .pragma_update(None, "user_version", 2)
            .expect("wrong version");
        drop(connection);

        let error = SqliteThreadStore::open(&path)
            .err()
            .expect("wrong version rejected");
        assert_eq!(
            error.to_string(),
            "App Server thread-store schema version 2 does not match required version 1"
        );
    }

    #[test]
    fn opening_a_malformed_current_schema_is_rejected() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("malformed.sqlite");
        let connection = Connection::open(&path).expect("connection");
        connection
            .execute_batch(
                "PRAGMA user_version = 1; CREATE TABLE app_server_threads (thread_id TEXT PRIMARY KEY);",
            )
            .expect("malformed schema");
        drop(connection);

        let error = SqliteThreadStore::open(&path)
            .err()
            .expect("malformed schema rejected");
        assert!(error
            .to_string()
            .contains("schema objects do not match the current schema"));
    }
}
