use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::types::{Message, MessageRole, ToolCall};

mod conformance;
mod redis_store;

pub use conformance::session_store_conformance;
pub use redis_store::RedisSessionStore;

pub trait Session: Send + Sync {
    fn session_id(&self) -> &str;
    fn get_items(&self, limit: Option<usize>) -> SessionFuture<Vec<SessionItem>>;
    fn add_items(&self, items: Vec<SessionItem>) -> SessionFuture<()>;
    fn pop_item(&self) -> SessionFuture<Option<SessionItem>>;
    fn clear(&self) -> SessionFuture<()>;

    fn supports_add_items_once(&self) -> bool {
        false
    }

    fn add_items_once(
        &self,
        _commit_id: String,
        _payload_digest: String,
        _items: Vec<SessionItem>,
    ) -> SessionFuture<SessionAppendOutcome> {
        Box::pin(async {
            Err("checkpoint_session_idempotency_unsupported: session does not support add_items_once"
                .to_string())
        })
    }

    fn clear_session(&self) -> SessionFuture<()> {
        self.clear()
    }
}
pub type SessionFuture<T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, String>> + Send>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionAppendOutcome {
    Committed,
    Replayed,
}

pub fn checkpoint_session_commit_id(checkpoint_key: &str) -> String {
    let digest = Sha256::digest(checkpoint_key.as_bytes());
    format!("vv-agent:checkpoint-v2:session:{digest:x}")
}

pub fn session_commit_payload_digest(items: &[SessionItem]) -> Result<String, String> {
    let payload = serde_json::json!({
        "schema_version": "vv-agent.session-commit.v1",
        "items": items,
    });
    let bytes = crate::checkpoint::canonical_json_bytes(&payload, "session commit payload")
        .map_err(|error| error.to_string())?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn validate_session_commit(
    commit_id: &str,
    payload_digest: &str,
    items: &[SessionItem],
) -> Result<(), String> {
    if commit_id.trim().is_empty() {
        return Err("session_commit_identity_invalid: commit_id must be non-empty".to_string());
    }
    if payload_digest.len() != 64
        || !payload_digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(
            "session_commit_payload_digest_invalid: payload_digest must be lowercase SHA-256"
                .to_string(),
        );
    }
    if session_commit_payload_digest(items)? != payload_digest {
        return Err(
            "session_commit_payload_digest_mismatch: payload_digest does not match items"
                .to_string(),
        );
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
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
    Message {
        message: Message,
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
            Self::Message { message } => message.clone(),
        }
    }

    pub fn from_message(message: &Message) -> Option<Self> {
        let has_unrepresentable_tool_call_id = match message.role {
            MessageRole::Tool => message.tool_call_id.is_none(),
            _ => message.tool_call_id.is_some(),
        };
        if has_unrepresentable_tool_call_id
            || message.name.is_some()
            || !message.tool_calls.is_empty()
            || message.reasoning_content.is_some()
            || message.image_url.is_some()
            || !message.metadata.is_empty()
        {
            return Some(Self::Message {
                message: message.clone(),
            });
        }
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

impl Serialize for SessionItem {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let message = self.to_message();
        SessionMessageWire(&message).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SessionItem {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let wire = serde_json::from_value::<SessionMessageInput>(value)
            .map_err(serde::de::Error::custom)?;
        let message = wire.into_message().map_err(serde::de::Error::custom)?;
        SessionItem::from_message(&message)
            .ok_or_else(|| serde::de::Error::custom("unsupported session message role"))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionMessageInput {
    role: String,
    content: String,
    #[serde(default, deserialize_with = "deserialize_present_string")]
    name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_present_string")]
    tool_call_id: Option<String>,
    #[serde(default)]
    tool_calls: Vec<SessionToolCallInput>,
    #[serde(default, deserialize_with = "deserialize_present_string")]
    reasoning_content: Option<String>,
    #[serde(default, deserialize_with = "deserialize_present_string")]
    image_url: Option<String>,
    #[serde(default)]
    metadata: BTreeMap<String, Value>,
}

impl SessionMessageInput {
    fn into_message(self) -> Result<Message, String> {
        let role = match self.role.as_str() {
            "system" => MessageRole::System,
            "user" => MessageRole::User,
            "assistant" => MessageRole::Assistant,
            "tool" => MessageRole::Tool,
            value => return Err(format!("unknown message role: {value}")),
        };
        let tool_calls = self
            .tool_calls
            .into_iter()
            .map(SessionToolCallInput::into_tool_call)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Message {
            role,
            content: self.content,
            name: self.name,
            tool_call_id: self.tool_call_id,
            tool_calls,
            reasoning_content: self.reasoning_content,
            image_url: self.image_url,
            metadata: self.metadata,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionToolCallInput {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    function: SessionToolFunctionInput,
    #[serde(default, deserialize_with = "deserialize_present_object")]
    extra_content: Option<BTreeMap<String, Value>>,
}

impl SessionToolCallInput {
    fn into_tool_call(self) -> Result<ToolCall, String> {
        if self.id.trim().is_empty() {
            return Err("session tool call id must be non-empty".to_string());
        }
        if self.kind != "function" {
            return Err(format!("unknown session tool call type: {}", self.kind));
        }
        if self.function.name.trim().is_empty() {
            return Err("session tool function name must be non-empty".to_string());
        }
        let arguments = serde_json::from_str::<Value>(&self.function.arguments)
            .map_err(|error| format!("invalid session tool arguments JSON: {error}"))?;
        let Value::Object(arguments) = arguments else {
            return Err("session tool arguments must decode to an object".to_string());
        };
        Ok(ToolCall {
            id: self.id,
            name: self.function.name,
            arguments: arguments.into_iter().collect(),
            extra_content: self
                .extra_content
                .map(|value| Value::Object(value.into_iter().collect())),
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionToolFunctionInput {
    name: String,
    arguments: String,
}

fn deserialize_present_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    String::deserialize(deserializer).map(Some)
}

fn deserialize_present_object<'de, D>(
    deserializer: D,
) -> Result<Option<BTreeMap<String, Value>>, D::Error>
where
    D: Deserializer<'de>,
{
    BTreeMap::<String, Value>::deserialize(deserializer).map(Some)
}

struct SessionMessageWire<'a>(&'a Message);

impl Serialize for SessionMessageWire<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let message = self.0;
        let mut field_count = 2;
        field_count += usize::from(message.name.is_some());
        field_count += usize::from(message.tool_call_id.is_some());
        field_count += usize::from(!message.tool_calls.is_empty());
        field_count += usize::from(message.reasoning_content.is_some());
        field_count += usize::from(message.image_url.is_some());
        field_count += usize::from(!message.metadata.is_empty());

        let mut state = serializer.serialize_map(Some(field_count))?;
        state.serialize_entry("role", &message.role)?;
        state.serialize_entry("content", &message.content)?;
        if let Some(name) = &message.name {
            state.serialize_entry("name", name)?;
        }
        if let Some(tool_call_id) = &message.tool_call_id {
            state.serialize_entry("tool_call_id", tool_call_id)?;
        }
        if !message.tool_calls.is_empty() {
            state.serialize_entry("tool_calls", &SessionToolCallsWire(&message.tool_calls))?;
        }
        if let Some(reasoning_content) = &message.reasoning_content {
            state.serialize_entry("reasoning_content", reasoning_content)?;
        }
        if let Some(image_url) = &message.image_url {
            state.serialize_entry("image_url", image_url)?;
        }
        if !message.metadata.is_empty() {
            state.serialize_entry("metadata", &message.metadata)?;
        }
        state.end()
    }
}

struct SessionToolCallsWire<'a>(&'a [ToolCall]);

impl Serialize for SessionToolCallsWire<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for tool_call in self.0 {
            sequence.serialize_element(&SessionToolCallWire(tool_call))?;
        }
        sequence.end()
    }
}

struct SessionToolCallWire<'a>(&'a ToolCall);

impl Serialize for SessionToolCallWire<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let tool_call = self.0;
        let field_count = 3 + usize::from(tool_call.extra_content.is_some());
        let mut state = serializer.serialize_map(Some(field_count))?;
        state.serialize_entry("id", &tool_call.id)?;
        state.serialize_entry("type", "function")?;
        state.serialize_entry("function", &SessionToolFunctionWire(tool_call))?;
        if let Some(extra_content) = &tool_call.extra_content {
            state.serialize_entry("extra_content", extra_content)?;
        }
        state.end()
    }
}

struct SessionToolFunctionWire<'a>(&'a ToolCall);

impl Serialize for SessionToolFunctionWire<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_map(Some(2))?;
        state.serialize_entry("name", &self.0.name)?;
        let arguments =
            serde_json::to_string(&self.0.arguments).map_err(serde::ser::Error::custom)?;
        state.serialize_entry("arguments", &arguments)?;
        state.end()
    }
}

#[derive(Clone)]
pub struct MemorySession {
    session_id: Arc<String>,
    items: Arc<Mutex<Vec<SessionItem>>>,
    commits: Arc<Mutex<HashMap<String, String>>>,
}

impl MemorySession {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: Arc::new(session_id.into()),
            items: Arc::new(Mutex::new(Vec::new())),
            commits: Arc::new(Mutex::new(HashMap::new())),
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

    fn supports_add_items_once(&self) -> bool {
        true
    }

    fn add_items_once(
        &self,
        commit_id: String,
        payload_digest: String,
        new_items: Vec<SessionItem>,
    ) -> SessionFuture<SessionAppendOutcome> {
        let items = self.items.clone();
        let commits = self.commits.clone();
        Box::pin(async move {
            validate_session_commit(&commit_id, &payload_digest, &new_items)?;
            let mut commits = commits
                .lock()
                .map_err(|_| "session commit lock poisoned".to_string())?;
            if let Some(existing) = commits.get(&commit_id) {
                if existing != &payload_digest {
                    return Err(
                        "session_commit_identity_conflict: commit_id has a different payload"
                            .to_string(),
                    );
                }
                return Ok(SessionAppendOutcome::Replayed);
            }
            items
                .lock()
                .map_err(|_| "session lock poisoned".to_string())?
                .extend(new_items);
            commits.insert(commit_id, payload_digest);
            Ok(SessionAppendOutcome::Committed)
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
        let commits = self.commits.clone();
        Box::pin(async move {
            items
                .lock()
                .map_err(|_| "session lock poisoned".to_string())?
                .clear();
            commits
                .lock()
                .map_err(|_| "session commit lock poisoned".to_string())?
                .clear();
            Ok(())
        })
    }
}

pub trait SessionStore: Send + Sync {
    fn session(&self, session_id: &str) -> Arc<dyn Session>;
}

#[derive(Clone, Default)]
pub struct MemorySessionStore {
    sessions: Arc<Mutex<HashMap<String, Arc<dyn Session>>>>,
}

impl MemorySessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn session(&self, session_id: &str) -> Arc<dyn Session> {
        <Self as SessionStore>::session(self, session_id)
    }
}

impl SessionStore for MemorySessionStore {
    fn session(&self, session_id: &str) -> Arc<dyn Session> {
        let mut sessions = self
            .sessions
            .lock()
            .expect("memory session store lock poisoned");
        sessions
            .entry(session_id.to_string())
            .or_insert_with(|| Arc::new(MemorySession::new(session_id)))
            .clone()
    }
}

#[derive(Clone)]
pub struct SqliteSessionStore {
    connection: Arc<Mutex<Connection>>,
}

const SQLITE_SESSION_SCHEMA_VERSION: i64 = 1;
const CANONICAL_SESSION_COLUMNS: [&str; 3] = ["session_id", "item_index", "payload"];
const CANONICAL_SESSION_COMMIT_COLUMNS: [&str; 3] = ["session_id", "commit_id", "payload_digest"];
const CREATE_SESSION_ITEMS_TABLE: &str = r#"
    CREATE TABLE IF NOT EXISTS session_items (
        session_id TEXT NOT NULL,
        item_index INTEGER PRIMARY KEY AUTOINCREMENT,
        payload TEXT NOT NULL
    )
"#;
const CREATE_SESSION_ITEMS_INDEX: &str = r#"
    CREATE INDEX IF NOT EXISTS idx_session_items_session_id_item_index
        ON session_items (session_id, item_index)
"#;
const CREATE_SESSION_COMMITS_TABLE: &str = r#"
    CREATE TABLE IF NOT EXISTS session_commits (
        session_id TEXT NOT NULL,
        commit_id TEXT NOT NULL,
        payload_digest TEXT NOT NULL,
        PRIMARY KEY (session_id, commit_id)
    )
"#;

impl SqliteSessionStore {
    pub fn open_memory() -> Result<Self, String> {
        Self::open(":memory:")
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        let mut connection = Connection::open(path).map_err(sqlite_error)?;
        connection
            .execute_batch(
                r#"
                PRAGMA busy_timeout = 5000;
                PRAGMA journal_mode=WAL;
                "#,
            )
            .map_err(sqlite_error)?;
        initialize_sqlite_session_schema(&mut connection)?;
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
                        SELECT payload
                        FROM (
                            SELECT item_index, payload
                            FROM session_items
                            WHERE session_id = ?1
                            ORDER BY item_index DESC
                            LIMIT ?2
                        )
                        ORDER BY item_index ASC
                        "#,
                    )
                    .map_err(sqlite_error)?
            } else {
                connection
                    .prepare(
                        r#"
                        SELECT payload
                        FROM session_items
                        WHERE session_id = ?1
                        ORDER BY item_index ASC
                        "#,
                    )
                    .map_err(sqlite_error)?
            };
            let mut rows = if let Some(limit) = limit {
                statement
                    .query(params![
                        session_id,
                        i64::try_from(limit).unwrap_or(i64::MAX)
                    ])
                    .map_err(sqlite_error)?
            } else {
                statement.query(params![session_id]).map_err(sqlite_error)?
            };
            let mut items = Vec::new();
            while let Some(row) = rows.next().map_err(sqlite_error)? {
                let payload: String = row.get(0).map_err(sqlite_error)?;
                items.push(serde_json::from_str(&payload).map_err(json_error)?);
            }
            Ok(items)
        })
    }

    fn add_items(&self, items: Vec<SessionItem>) -> SessionFuture<()> {
        let session_id = self.session_id.to_string();
        let connection = self.connection.clone();
        Box::pin(async move {
            if items.is_empty() {
                return Ok(());
            }
            let mut connection = connection
                .lock()
                .map_err(|_| "sqlite session store lock poisoned".to_string())?;
            let transaction = connection.transaction().map_err(sqlite_error)?;
            for item in items {
                let payload = serde_json::to_string(&item).map_err(json_error)?;
                transaction
                    .execute(
                        "INSERT INTO session_items (session_id, payload) VALUES (?1, ?2)",
                        params![session_id, payload],
                    )
                    .map_err(sqlite_error)?;
            }
            transaction.commit().map_err(sqlite_error)?;
            Ok(())
        })
    }

    fn supports_add_items_once(&self) -> bool {
        true
    }

    fn add_items_once(
        &self,
        commit_id: String,
        payload_digest: String,
        items: Vec<SessionItem>,
    ) -> SessionFuture<SessionAppendOutcome> {
        let session_id = self.session_id.to_string();
        let connection = self.connection.clone();
        Box::pin(async move {
            validate_session_commit(&commit_id, &payload_digest, &items)?;
            let payloads = items
                .iter()
                .map(serde_json::to_string)
                .collect::<Result<Vec<_>, _>>()
                .map_err(json_error)?;
            let mut connection = connection
                .lock()
                .map_err(|_| "sqlite session store lock poisoned".to_string())?;
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(sqlite_error)?;
            let existing = transaction
                .query_row(
                    "SELECT payload_digest FROM session_commits WHERE session_id = ?1 AND commit_id = ?2",
                    params![session_id, commit_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(sqlite_error)?;
            if let Some(existing) = existing {
                if existing != payload_digest {
                    return Err(
                        "session_commit_identity_conflict: commit_id has a different payload"
                            .to_string(),
                    );
                }
                transaction.commit().map_err(sqlite_error)?;
                return Ok(SessionAppendOutcome::Replayed);
            }
            for payload in payloads {
                transaction
                    .execute(
                        "INSERT INTO session_items (session_id, payload) VALUES (?1, ?2)",
                        params![session_id, payload],
                    )
                    .map_err(sqlite_error)?;
            }
            transaction
                .execute(
                    "INSERT INTO session_commits (session_id, commit_id, payload_digest) VALUES (?1, ?2, ?3)",
                    params![session_id, commit_id, payload_digest],
                )
                .map_err(sqlite_error)?;
            transaction.commit().map_err(sqlite_error)?;
            Ok(SessionAppendOutcome::Committed)
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
                    SELECT item_index, payload
                    FROM session_items
                    WHERE session_id = ?1
                    ORDER BY item_index DESC
                    LIMIT 1
                    "#,
                    params![session_id],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(sqlite_error)?;
            let Some((item_index, payload)) = row else {
                transaction.commit().map_err(sqlite_error)?;
                return Ok(None);
            };
            let item = serde_json::from_str(&payload).map_err(json_error)?;
            transaction
                .execute(
                    "DELETE FROM session_items WHERE item_index = ?1",
                    params![item_index],
                )
                .map_err(sqlite_error)?;
            transaction.commit().map_err(sqlite_error)?;
            Ok(Some(item))
        })
    }

    fn clear(&self) -> SessionFuture<()> {
        let session_id = self.session_id.to_string();
        let connection = self.connection.clone();
        Box::pin(async move {
            let mut connection = connection
                .lock()
                .map_err(|_| "sqlite session store lock poisoned".to_string())?;
            let transaction = connection.transaction().map_err(sqlite_error)?;
            transaction
                .execute(
                    "DELETE FROM session_items WHERE session_id = ?1",
                    params![session_id],
                )
                .map_err(sqlite_error)?;
            transaction
                .execute(
                    "DELETE FROM session_commits WHERE session_id = ?1",
                    params![session_id],
                )
                .map_err(sqlite_error)?;
            transaction.commit().map_err(sqlite_error)?;
            Ok(())
        })
    }
}

fn initialize_sqlite_session_schema(connection: &mut Connection) -> Result<(), String> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    let version = transaction
        .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
        .map_err(sqlite_error)?;
    let tables = session_table_names(&transaction)?;
    if tables.is_empty() {
        if version != 0 {
            return Err(format!(
                "empty session database has unexpected schema version {version}"
            ));
        }
        transaction
            .execute_batch(CREATE_SESSION_ITEMS_TABLE)
            .map_err(sqlite_error)?;
        transaction
            .execute_batch(CREATE_SESSION_ITEMS_INDEX)
            .map_err(sqlite_error)?;
        transaction
            .execute_batch(CREATE_SESSION_COMMITS_TABLE)
            .map_err(sqlite_error)?;
        transaction
            .execute_batch("PRAGMA user_version = 1;")
            .map_err(sqlite_error)?;
    } else {
        if version != SQLITE_SESSION_SCHEMA_VERSION {
            return Err(format!(
                "session schema version {version} does not match required version \
                 {SQLITE_SESSION_SCHEMA_VERSION}"
            ));
        }
        if tables != ["session_commits".to_string(), "session_items".to_string()] {
            return Err(format!("unsupported session schema tables: {tables:?}"));
        }
        let item_columns = session_table_columns(&transaction, "session_items")?;
        if item_columns != CANONICAL_SESSION_COLUMNS {
            return Err(format!(
                "unsupported session_items schema columns: {item_columns:?}"
            ));
        }
        let commit_columns = session_table_columns(&transaction, "session_commits")?;
        if commit_columns != CANONICAL_SESSION_COMMIT_COLUMNS {
            return Err(format!(
                "unsupported session_commits schema columns: {commit_columns:?}"
            ));
        }
        let index_exists = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master \
                 WHERE type = 'index' AND name = 'idx_session_items_session_id_item_index')",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(sqlite_error)?
            != 0;
        if !index_exists {
            return Err("session schema is missing the canonical session_items index".to_string());
        }
    }
    transaction.commit().map_err(sqlite_error)
}

fn session_table_names(connection: &Connection) -> Result<Vec<String>, String> {
    let mut statement = connection
        .prepare(
            "SELECT name FROM sqlite_master \
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(sqlite_error)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sqlite_error)
}

fn session_table_columns(connection: &Connection, table: &str) -> Result<Vec<String>, String> {
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(sqlite_error)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sqlite_error)
}

fn sqlite_error(error: rusqlite::Error) -> String {
    error.to_string()
}

fn json_error(error: serde_json::Error) -> String {
    error.to_string()
}
