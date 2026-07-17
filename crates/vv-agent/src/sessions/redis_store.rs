use redis::{Commands, Connection as RedisConnection};

use super::*;

const REDIS_SESSION_KEY_PREFIX: &str = "vv-agent-session";

#[derive(Clone)]
pub struct RedisSessionStore {
    connection: Arc<Mutex<RedisConnection>>,
    key_prefix: Arc<String>,
}

impl RedisSessionStore {
    pub fn new(redis_url: impl AsRef<str>) -> Result<Self, String> {
        Self::with_key_prefix(redis_url, REDIS_SESSION_KEY_PREFIX)
    }

    pub fn with_key_prefix(
        redis_url: impl AsRef<str>,
        key_prefix: impl Into<String>,
    ) -> Result<Self, String> {
        let client = redis::Client::open(redis_url.as_ref()).map_err(redis_error)?;
        let connection = client.get_connection().map_err(redis_error)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            key_prefix: Arc::new(key_prefix.into()),
        })
    }

    pub fn session(&self, session_id: &str) -> Arc<dyn Session> {
        <Self as SessionStore>::session(self, session_id)
    }
}

impl SessionStore for RedisSessionStore {
    fn session(&self, session_id: &str) -> Arc<dyn Session> {
        Arc::new(RedisSession {
            session_id: Arc::new(session_id.to_string()),
            connection: self.connection.clone(),
            key_prefix: self.key_prefix.clone(),
        })
    }
}

#[derive(Clone)]
struct RedisSession {
    session_id: Arc<String>,
    connection: Arc<Mutex<RedisConnection>>,
    key_prefix: Arc<String>,
}

impl RedisSession {
    fn key(&self) -> String {
        format!("{}:{}", self.key_prefix, self.session_id)
    }

    fn commit_key(&self) -> String {
        format!("{}:commits", self.key())
    }
}

impl Session for RedisSession {
    fn session_id(&self) -> &str {
        self.session_id.as_str()
    }

    fn get_items(&self, limit: Option<usize>) -> SessionFuture<Vec<SessionItem>> {
        let connection = self.connection.clone();
        let key = self.key();
        Box::pin(async move {
            let raw_items: Vec<String> = {
                let mut connection = connection
                    .lock()
                    .map_err(|_| "redis session store lock poisoned".to_string())?;
                match limit {
                    Some(0) => return Ok(Vec::new()),
                    Some(limit) => {
                        let limit = isize::try_from(limit).unwrap_or(isize::MAX);
                        connection.lrange(&key, -limit, -1).map_err(redis_error)?
                    }
                    None => connection.lrange(&key, 0, -1).map_err(redis_error)?,
                }
            };
            raw_items
                .into_iter()
                .map(|item_json| serde_json::from_str(&item_json).map_err(json_error))
                .collect()
        })
    }

    fn add_items(&self, items: Vec<SessionItem>) -> SessionFuture<()> {
        let connection = self.connection.clone();
        let key = self.key();
        Box::pin(async move {
            if items.is_empty() {
                return Ok(());
            }
            let payloads = items
                .iter()
                .map(serde_json::to_string)
                .collect::<Result<Vec<_>, _>>()
                .map_err(json_error)?;
            connection
                .lock()
                .map_err(|_| "redis session store lock poisoned".to_string())?
                .rpush::<_, _, usize>(key, payloads)
                .map_err(redis_error)?;
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
        let connection = self.connection.clone();
        let items_key = self.key();
        let commits_key = self.commit_key();
        Box::pin(async move {
            validate_session_commit(&commit_id, &payload_digest, &items)?;
            let payloads = items
                .iter()
                .map(serde_json::to_string)
                .collect::<Result<Vec<_>, _>>()
                .map_err(json_error)?;
            let script = redis::Script::new(
                r#"
                local existing = redis.call('HGET', KEYS[2], ARGV[1])
                if existing then
                    if existing == ARGV[2] then
                        return 0
                    end
                    return -1
                end
                for index = 3, #ARGV do
                    redis.call('RPUSH', KEYS[1], ARGV[index])
                end
                redis.call('HSET', KEYS[2], ARGV[1], ARGV[2])
                return 1
                "#,
            );
            let mut invocation = script.prepare_invoke();
            invocation.key(items_key).key(commits_key);
            invocation.arg(commit_id).arg(payload_digest);
            for payload in payloads {
                invocation.arg(payload);
            }
            let outcome: i64 = invocation
                .invoke(
                    &mut *connection
                        .lock()
                        .map_err(|_| "redis session store lock poisoned".to_string())?,
                )
                .map_err(redis_error)?;
            match outcome {
                1 => Ok(SessionAppendOutcome::Committed),
                0 => Ok(SessionAppendOutcome::Replayed),
                -1 => Err(
                    "session_commit_identity_conflict: commit_id has a different payload"
                        .to_string(),
                ),
                _ => Err("redis session append-once returned an invalid outcome".to_string()),
            }
        })
    }

    fn pop_item(&self) -> SessionFuture<Option<SessionItem>> {
        let connection = self.connection.clone();
        let key = self.key();
        Box::pin(async move {
            let raw: Option<String> = connection
                .lock()
                .map_err(|_| "redis session store lock poisoned".to_string())?
                .rpop(key, None)
                .map_err(redis_error)?;
            raw.map(|item_json| serde_json::from_str(&item_json).map_err(json_error))
                .transpose()
        })
    }

    fn clear(&self) -> SessionFuture<()> {
        let connection = self.connection.clone();
        let key = self.key();
        let commit_key = self.commit_key();
        Box::pin(async move {
            redis::cmd("DEL")
                .arg(key)
                .arg(commit_key)
                .query::<usize>(
                    &mut *connection
                        .lock()
                        .map_err(|_| "redis session store lock poisoned".to_string())?,
                )
                .map_err(redis_error)?;
            Ok(())
        })
    }
}

fn redis_error(error: redis::RedisError) -> String {
    error.to_string()
}
