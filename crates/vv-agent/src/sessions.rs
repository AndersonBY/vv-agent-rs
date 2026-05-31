use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection, OptionalExtension};

use crate::types::{Message, MessageRole};

pub trait Session: Send + Sync {
    fn session_id(&self) -> &str;
    fn get_items(&self, limit: Option<usize>) -> SessionFuture<Vec<SessionItem>>;
    fn add_items(&self, items: Vec<SessionItem>) -> SessionFuture<()>;
    fn pop_item(&self) -> SessionFuture<Option<SessionItem>>;
    fn clear(&self) -> SessionFuture<()>;
}

pub type SessionFuture<T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, String>> + Send>>;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionItem {
    User {
        content: String,
    },
    Assistant {
        content: String,
    },
    System {
        content: String,
    },
    Tool {
        content: String,
        tool_call_id: String,
    },
}

impl SessionItem {
    pub fn to_message(&self) -> Message {
        match self {
            Self::User { content } => Message::user(content.clone()),
            Self::Assistant { content } => Message::assistant(content.clone()),
            Self::System { content } => Message::system(content.clone()),
            Self::Tool {
                content,
                tool_call_id,
            } => Message::tool(content.clone(), tool_call_id.clone()),
        }
    }

    pub fn from_message(message: &Message) -> Option<Self> {
        match message.role {
            MessageRole::System => Some(Self::System {
                content: message.content.clone(),
            }),
            MessageRole::User => Some(Self::User {
                content: message.content.clone(),
            }),
            MessageRole::Assistant => Some(Self::Assistant {
                content: message.content.clone(),
            }),
            MessageRole::Tool => Some(Self::Tool {
                content: message.content.clone(),
                tool_call_id: message.tool_call_id.clone().unwrap_or_default(),
            }),
        }
    }
}

#[derive(Clone)]
pub struct MemorySession {
    session_id: Arc<String>,
    items: Arc<Mutex<Vec<SessionItem>>>,
}

impl MemorySession {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: Arc::new(session_id.into()),
            items: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl Session for MemorySession {
    fn session_id(&self) -> &str {
        self.session_id.as_str()
    }

    fn get_items(&self, limit: Option<usize>) -> SessionFuture<Vec<SessionItem>> {
        let items = self.items.clone();
        Box::pin(async move {
            let items = items
                .lock()
                .map_err(|_| "session lock poisoned".to_string())?;
            let values = match limit {
                Some(limit) => items
                    .iter()
                    .rev()
                    .take(limit)
                    .cloned()
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect(),
                None => items.clone(),
            };
            Ok(values)
        })
    }

    fn add_items(&self, new_items: Vec<SessionItem>) -> SessionFuture<()> {
        let items = self.items.clone();
        Box::pin(async move {
            items
                .lock()
                .map_err(|_| "session lock poisoned".to_string())?
                .extend(new_items);
            Ok(())
        })
    }

    fn pop_item(&self) -> SessionFuture<Option<SessionItem>> {
        let items = self.items.clone();
        Box::pin(async move {
            Ok(items
                .lock()
                .map_err(|_| "session lock poisoned".to_string())?
                .pop())
        })
    }

    fn clear(&self) -> SessionFuture<()> {
        let items = self.items.clone();
        Box::pin(async move {
            items
                .lock()
                .map_err(|_| "session lock poisoned".to_string())?
                .clear();
            Ok(())
        })
    }
}

pub trait SessionStore: Send + Sync {
    fn session(&self, session_id: &str) -> Arc<dyn Session>;
}

#[derive(Clone)]
pub struct SqliteSessionStore {
    connection: Arc<Mutex<Connection>>,
}

impl SqliteSessionStore {
    pub fn open_memory() -> Result<Self, String> {
        Self::open(":memory:")
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        let connection = Connection::open(path).map_err(sqlite_error)?;
        connection
            .execute_batch(
                r#"
                PRAGMA journal_mode=WAL;
                CREATE TABLE IF NOT EXISTS session_items (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    session_id TEXT NOT NULL,
                    item_json TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_session_items_session_id_id
                    ON session_items (session_id, id);
                "#,
            )
            .map_err(sqlite_error)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    pub fn session(&self, session_id: &str) -> Arc<dyn Session> {
        <Self as SessionStore>::session(self, session_id)
    }
}

impl SessionStore for SqliteSessionStore {
    fn session(&self, session_id: &str) -> Arc<dyn Session> {
        Arc::new(SqliteSession {
            session_id: Arc::new(session_id.to_string()),
            connection: self.connection.clone(),
        })
    }
}

#[derive(Clone)]
struct SqliteSession {
    session_id: Arc<String>,
    connection: Arc<Mutex<Connection>>,
}

impl Session for SqliteSession {
    fn session_id(&self) -> &str {
        self.session_id.as_str()
    }

    fn get_items(&self, limit: Option<usize>) -> SessionFuture<Vec<SessionItem>> {
        let session_id = self.session_id.to_string();
        let connection = self.connection.clone();
        Box::pin(async move {
            let connection = connection
                .lock()
                .map_err(|_| "sqlite session store lock poisoned".to_string())?;
            let mut statement = if limit.is_some() {
                connection
                    .prepare(
                        r#"
                        SELECT item_json
                        FROM (
                            SELECT id, item_json
                            FROM session_items
                            WHERE session_id = ?1
                            ORDER BY id DESC
                            LIMIT ?2
                        )
                        ORDER BY id ASC
                        "#,
                    )
                    .map_err(sqlite_error)?
            } else {
                connection
                    .prepare(
                        r#"
                        SELECT item_json
                        FROM session_items
                        WHERE session_id = ?1
                        ORDER BY id ASC
                        "#,
                    )
                    .map_err(sqlite_error)?
            };
            let mut rows = if let Some(limit) = limit {
                statement
                    .query(params![session_id, limit as i64])
                    .map_err(sqlite_error)?
            } else {
                statement.query(params![session_id]).map_err(sqlite_error)?
            };
            let mut items = Vec::new();
            while let Some(row) = rows.next().map_err(sqlite_error)? {
                let item_json: String = row.get(0).map_err(sqlite_error)?;
                items.push(serde_json::from_str(&item_json).map_err(json_error)?);
            }
            Ok(items)
        })
    }

    fn add_items(&self, items: Vec<SessionItem>) -> SessionFuture<()> {
        let session_id = self.session_id.to_string();
        let connection = self.connection.clone();
        Box::pin(async move {
            let mut connection = connection
                .lock()
                .map_err(|_| "sqlite session store lock poisoned".to_string())?;
            let transaction = connection.transaction().map_err(sqlite_error)?;
            for item in items {
                let item_json = serde_json::to_string(&item).map_err(json_error)?;
                transaction
                    .execute(
                        "INSERT INTO session_items (session_id, item_json) VALUES (?1, ?2)",
                        params![session_id, item_json],
                    )
                    .map_err(sqlite_error)?;
            }
            transaction.commit().map_err(sqlite_error)?;
            Ok(())
        })
    }

    fn pop_item(&self) -> SessionFuture<Option<SessionItem>> {
        let session_id = self.session_id.to_string();
        let connection = self.connection.clone();
        Box::pin(async move {
            let mut connection = connection
                .lock()
                .map_err(|_| "sqlite session store lock poisoned".to_string())?;
            let transaction = connection.transaction().map_err(sqlite_error)?;
            let row = transaction
                .query_row(
                    r#"
                    SELECT id, item_json
                    FROM session_items
                    WHERE session_id = ?1
                    ORDER BY id DESC
                    LIMIT 1
                    "#,
                    params![session_id],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(sqlite_error)?;
            let Some((id, item_json)) = row else {
                transaction.commit().map_err(sqlite_error)?;
                return Ok(None);
            };
            transaction
                .execute("DELETE FROM session_items WHERE id = ?1", params![id])
                .map_err(sqlite_error)?;
            transaction.commit().map_err(sqlite_error)?;
            Ok(Some(serde_json::from_str(&item_json).map_err(json_error)?))
        })
    }

    fn clear(&self) -> SessionFuture<()> {
        let session_id = self.session_id.to_string();
        let connection = self.connection.clone();
        Box::pin(async move {
            connection
                .lock()
                .map_err(|_| "sqlite session store lock poisoned".to_string())?
                .execute(
                    "DELETE FROM session_items WHERE session_id = ?1",
                    params![session_id],
                )
                .map_err(sqlite_error)?;
            Ok(())
        })
    }
}

fn sqlite_error(error: rusqlite::Error) -> String {
    error.to_string()
}

fn json_error(error: serde_json::Error) -> String {
    error.to_string()
}

pub async fn session_store_conformance(store: &dyn SessionStore) -> Result<(), String> {
    let session = store.session("conformance-thread");
    session.clear().await?;
    session
        .add_items(vec![SessionItem::User {
            content: "hello".to_string(),
        }])
        .await?;
    let same_session = store.session("conformance-thread");
    let items = same_session.get_items(None).await?;
    if items
        != vec![SessionItem::User {
            content: "hello".to_string(),
        }]
    {
        return Err("session store did not persist appended items".to_string());
    }
    let popped = same_session.pop_item().await?;
    if !matches!(popped, Some(SessionItem::User { content }) if content == "hello") {
        return Err("session store pop_item returned unexpected item".to_string());
    }
    if !same_session.get_items(None).await?.is_empty() {
        return Err("session store pop_item did not remove the item".to_string());
    }
    Ok(())
}
