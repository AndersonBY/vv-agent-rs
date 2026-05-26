use std::io::{Error, Result};
use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension};

use crate::runtime::state::{
    checkpoint_status_from_value, checkpoint_status_value, from_json, to_json, Checkpoint,
    StateStore,
};

#[derive(Debug)]
pub struct SqliteStateStore {
    connection: Mutex<Connection>,
}

impl SqliteStateStore {
    pub fn new(db_path: impl AsRef<Path>) -> Result<Self> {
        let db_path = db_path.as_ref().to_string_lossy().to_string();
        let connection = Connection::open(db_path).map_err(sqlite_to_io)?;
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
                    shared_state TEXT NOT NULL
                );
                "#,
            )
            .map_err(sqlite_to_io)?;
        Ok(Self {
            connection: Mutex::new(connection),
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
    fn save_checkpoint(&self, checkpoint: Checkpoint) -> Result<()> {
        let messages_json = to_json(&checkpoint.messages)?;
        let cycles_json = to_json(&checkpoint.cycles)?;
        let shared_state_json = to_json(&checkpoint.shared_state)?;
        self.connection
            .lock()
            .map_err(|_| Error::other("sqlite state store lock is poisoned"))?
            .execute(
                r#"
                INSERT OR REPLACE INTO checkpoints
                    (task_id, cycle_index, status, messages, cycles, shared_state)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                "#,
                params![
                    checkpoint.task_id,
                    checkpoint.cycle_index,
                    checkpoint_status_value(checkpoint.status),
                    messages_json,
                    cycles_json,
                    shared_state_json,
                ],
            )
            .map_err(sqlite_to_io)?;
        Ok(())
    }

    fn load_checkpoint(&self, task_id: &str) -> Result<Option<Checkpoint>> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| Error::other("sqlite state store lock is poisoned"))?;
        let row = connection
            .query_row(
                "SELECT task_id, cycle_index, status, messages, cycles, shared_state FROM checkpoints WHERE task_id = ?1",
                params![task_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, u32>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()
            .map_err(sqlite_to_io)?;
        let Some((task_id, cycle_index, status, messages, cycles, shared_state)) = row else {
            return Ok(None);
        };
        Ok(Some(Checkpoint {
            task_id,
            cycle_index,
            status: checkpoint_status_from_value(&status)?,
            messages: from_json(&messages)?,
            cycles: from_json(&cycles)?,
            shared_state: from_json(&shared_state)?,
        }))
    }

    fn delete_checkpoint(&self, task_id: &str) -> Result<()> {
        self.connection
            .lock()
            .map_err(|_| Error::other("sqlite state store lock is poisoned"))?
            .execute(
                "DELETE FROM checkpoints WHERE task_id = ?1",
                params![task_id],
            )
            .map_err(sqlite_to_io)?;
        Ok(())
    }

    fn list_checkpoints(&self) -> Result<Vec<String>> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| Error::other("sqlite state store lock is poisoned"))?;
        let mut statement = connection
            .prepare("SELECT task_id FROM checkpoints ORDER BY task_id")
            .map_err(sqlite_to_io)?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(sqlite_to_io)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_to_io)
    }
}

fn sqlite_to_io(error: rusqlite::Error) -> Error {
    Error::other(error.to_string())
}
