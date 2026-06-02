use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};

use crate::app_server::protocol::{AppItem, AppThread, ThreadStartParams, ThreadStatus};

#[derive(Clone)]
pub struct SqliteThreadStore {
    connection: Arc<Mutex<Connection>>,
    next_thread_id: Arc<AtomicU64>,
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
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn create_thread(&self, params: ThreadStartParams) -> Result<AppThread, ThreadStoreError> {
        let sequence = self.next_thread_id.fetch_add(1, Ordering::Relaxed);
        let now = timestamp_millis();
        let thread = AppThread {
            id: format!("thread_{sequence}"),
            title: params.title,
            cwd: params.cwd,
            model: params.model,
            status: ThreadStatus::Idle,
            archived: false,
            ephemeral: params.ephemeral,
            created_at_ms: now,
            updated_at_ms: now + sequence as u128,
            active_turn_id: None,
        };
        self.insert_thread(&thread)?;
        Ok(thread)
    }

    pub fn get_thread(&self, thread_id: &str) -> Result<Option<AppThread>, ThreadStoreError> {
        let connection = self.connection.lock().map_err(ThreadStoreError::poisoned)?;
        connection
            .query_row(
                "SELECT id, title, cwd, model, status, archived, ephemeral, created_at_ms, updated_at_ms, active_turn_id
                 FROM threads WHERE id = ?1",
                params![thread_id],
                row_to_thread,
            )
            .optional()
            .map_err(ThreadStoreError::sql)
    }

    pub fn list_threads(&self, include_archived: bool) -> Result<Vec<AppThread>, ThreadStoreError> {
        let connection = self.connection.lock().map_err(ThreadStoreError::poisoned)?;
        let sql = if include_archived {
            "SELECT id, title, cwd, model, status, archived, ephemeral, created_at_ms, updated_at_ms, active_turn_id
             FROM threads ORDER BY updated_at_ms DESC, id DESC"
        } else {
            "SELECT id, title, cwd, model, status, archived, ephemeral, created_at_ms, updated_at_ms, active_turn_id
             FROM threads WHERE archived = 0 ORDER BY updated_at_ms DESC, id DESC"
        };
        let mut statement = connection.prepare(sql).map_err(ThreadStoreError::sql)?;
        let rows = statement
            .query_map([], row_to_thread)
            .map_err(ThreadStoreError::sql)?;
        let mut threads = Vec::new();
        for row in rows {
            threads.push(row.map_err(ThreadStoreError::sql)?);
        }
        Ok(threads)
    }

    pub fn archive_thread(&self, thread_id: &str) -> Result<(), ThreadStoreError> {
        let now = timestamp_millis();
        let connection = self.connection.lock().map_err(ThreadStoreError::poisoned)?;
        connection
            .execute(
                "UPDATE threads
                 SET archived = 1, status = 'archived', updated_at_ms = ?2
                 WHERE id = ?1",
                params![thread_id, now as i64],
            )
            .map_err(ThreadStoreError::sql)?;
        Ok(())
    }

    pub fn set_active_turn(
        &self,
        thread_id: &str,
        active_turn_id: Option<&str>,
        status: ThreadStatus,
    ) -> Result<(), ThreadStoreError> {
        let now = timestamp_millis();
        let connection = self.connection.lock().map_err(ThreadStoreError::poisoned)?;
        connection
            .execute(
                "UPDATE threads
                 SET active_turn_id = ?2, status = ?3, updated_at_ms = ?4
                 WHERE id = ?1",
                params![
                    thread_id,
                    active_turn_id,
                    thread_status_to_str(status),
                    now as i64
                ],
            )
            .map_err(ThreadStoreError::sql)?;
        Ok(())
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
                "SELECT COALESCE(MAX(sequence), 0) + 1 FROM thread_items WHERE thread_id = ?1",
                params![thread_id],
                |row| row.get(0),
            )
            .map_err(ThreadStoreError::sql)?;
        connection
            .execute(
                "INSERT INTO thread_items (id, thread_id, turn_id, run_event_id, sequence, payload_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    item.id,
                    thread_id,
                    turn_id,
                    item.run_event_id,
                    sequence,
                    payload_json
                ],
            )
            .map_err(ThreadStoreError::sql)?;
        Ok(())
    }

    pub fn replay_items(&self, thread_id: &str) -> Result<Vec<AppItem>, ThreadStoreError> {
        let connection = self.connection.lock().map_err(ThreadStoreError::poisoned)?;
        let mut statement = connection
            .prepare(
                "SELECT payload_json FROM thread_items
                 WHERE thread_id = ?1
                 ORDER BY sequence ASC",
            )
            .map_err(ThreadStoreError::sql)?;
        let rows = statement
            .query_map(params![thread_id], |row| row.get::<_, String>(0))
            .map_err(ThreadStoreError::sql)?;
        let mut items = Vec::new();
        for row in rows {
            let payload_json = row.map_err(ThreadStoreError::sql)?;
            items.push(serde_json::from_str(&payload_json).map_err(ThreadStoreError::json)?);
        }
        Ok(items)
    }

    fn insert_thread(&self, thread: &AppThread) -> Result<(), ThreadStoreError> {
        let cwd = thread
            .cwd
            .as_ref()
            .map(|path| path_to_string(path.as_path()));
        let connection = self.connection.lock().map_err(ThreadStoreError::poisoned)?;
        connection
            .execute(
                "INSERT INTO threads (
                    id, title, cwd, model, status, archived, ephemeral,
                    created_at_ms, updated_at_ms, active_turn_id
                 )
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    thread.id,
                    thread.title,
                    cwd,
                    thread.model,
                    thread_status_to_str(thread.status),
                    bool_to_i64(thread.archived),
                    bool_to_i64(thread.ephemeral),
                    thread.created_at_ms as i64,
                    thread.updated_at_ms as i64,
                    thread.active_turn_id,
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
                CREATE TABLE IF NOT EXISTS threads (
                    id TEXT PRIMARY KEY,
                    title TEXT,
                    cwd TEXT,
                    model TEXT,
                    status TEXT NOT NULL,
                    archived INTEGER NOT NULL,
                    ephemeral INTEGER NOT NULL,
                    created_at_ms INTEGER NOT NULL,
                    updated_at_ms INTEGER NOT NULL,
                    active_turn_id TEXT
                );

                CREATE TABLE IF NOT EXISTS thread_items (
                    id TEXT PRIMARY KEY,
                    thread_id TEXT NOT NULL,
                    turn_id TEXT NOT NULL,
                    run_event_id TEXT NOT NULL,
                    sequence INTEGER NOT NULL,
                    payload_json TEXT NOT NULL
                );

                CREATE INDEX IF NOT EXISTS idx_thread_items_thread_sequence
                    ON thread_items(thread_id, sequence);
                "#,
            )
            .map_err(ThreadStoreError::sql)?;
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
    let created_at_ms: i64 = row.get("created_at_ms")?;
    let updated_at_ms: i64 = row.get("updated_at_ms")?;
    Ok(AppThread {
        id: row.get("id")?,
        title: row.get("title")?,
        cwd: cwd.map(PathBuf::from),
        model: row.get("model")?,
        status: thread_status_from_str(&status).map_err(|message| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(ThreadStoreError { message }),
            )
        })?,
        archived: row.get::<_, i64>("archived")? != 0,
        ephemeral: row.get::<_, i64>("ephemeral")? != 0,
        created_at_ms: created_at_ms as u128,
        updated_at_ms: updated_at_ms as u128,
        active_turn_id: row.get("active_turn_id")?,
    })
}

fn thread_status_to_str(status: ThreadStatus) -> &'static str {
    match status {
        ThreadStatus::Idle => "idle",
        ThreadStatus::Running => "running",
        ThreadStatus::Archived => "archived",
    }
}

fn thread_status_from_str(status: &str) -> Result<ThreadStatus, String> {
    match status {
        "idle" => Ok(ThreadStatus::Idle),
        "running" => Ok(ThreadStatus::Running),
        "archived" => Ok(ThreadStatus::Archived),
        other => Err(format!("unknown thread status: {other}")),
    }
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn bool_to_i64(value: bool) -> i64 {
    if value {
        1
    } else {
        0
    }
}

fn timestamp_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}
