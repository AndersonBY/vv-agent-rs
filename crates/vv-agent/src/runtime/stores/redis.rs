use std::io::{Error, ErrorKind, Result};
use std::sync::Mutex;
use std::time::Duration;

use redis::{cmd, pipe, Commands, Connection, ConnectionLike, Pipeline, RedisResult};

use crate::runtime::checkpoint_codec;
use crate::runtime::state::{
    check_claim, claim_matches, clear_claim, validate_claim, validate_renew, Checkpoint,
    LeaseOperationClock, StateStore, StateStoreSpec,
};

const KEY_PREFIX: &str = "vv_agent:checkpoint:";
const IO_TIMEOUT: Duration = Duration::from_secs(1);
const TRANSACTION_MAX_ATTEMPTS: usize = 8;
const RENEW_CLAIM_SCRIPT: &str = r#"
local redis_time = redis.call("TIME")
local server_now_ms = tonumber(redis_time[1]) * 1000 + math.floor(tonumber(redis_time[2]) / 1000)
local client_now_ms = tonumber(ARGV[5])
local current_now_ms = math.max(server_now_ms, client_now_ms)
local previous_expiry_ms = tonumber(ARGV[3])
local requested_expiry_ms = tonumber(ARGV[4])
if previous_expiry_ms <= current_now_ms or requested_expiry_ms <= current_now_ms then
  return 2
end
local current = redis.call("GET", KEYS[1])
if current ~= ARGV[1] then
  return 0
end
redis.call("SET", KEYS[1], ARGV[2])
return 1
"#;

pub struct RedisStateStore {
    connection: Mutex<Connection>,
    redis_url: String,
}

impl std::fmt::Debug for RedisStateStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RedisStateStore")
            .finish_non_exhaustive()
    }
}

impl RedisStateStore {
    pub fn new(redis_url: impl AsRef<str>) -> Result<Self> {
        let redis_url = redis_url.as_ref().to_string();
        let client = redis::Client::open(redis_url.as_str()).map_err(redis_to_io)?;
        let connection = client
            .get_connection_with_timeout(IO_TIMEOUT)
            .map_err(redis_to_io)?;
        connection
            .set_read_timeout(Some(IO_TIMEOUT))
            .map_err(redis_to_io)?;
        connection
            .set_write_timeout(Some(IO_TIMEOUT))
            .map_err(redis_to_io)?;
        Ok(Self {
            connection: Mutex::new(connection),
            redis_url,
        })
    }

    pub fn checkpoint_key(task_id: &str) -> String {
        format!("{KEY_PREFIX}{task_id}")
    }

    pub fn checkpoint_to_json(checkpoint: &Checkpoint) -> Result<String> {
        checkpoint_codec::checkpoint_to_json(checkpoint)
    }

    pub fn checkpoint_from_json(raw: &str) -> Result<Checkpoint> {
        checkpoint_codec::checkpoint_from_json(raw)
    }

    fn transaction<T>(
        &self,
        key: &str,
        operation: impl FnMut(&mut Connection, &mut redis::Pipeline) -> redis::RedisResult<Option<T>>,
    ) -> Result<T> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| Error::other("redis state store lock is poisoned"))?;
        transaction_with_connection(&mut *connection, key, operation)
    }
}

fn transaction_with_connection<C, T>(
    connection: &mut C,
    key: &str,
    mut operation: impl FnMut(&mut C, &mut Pipeline) -> RedisResult<Option<T>>,
) -> Result<T>
where
    C: ConnectionLike,
{
    for _attempt in 0..TRANSACTION_MAX_ATTEMPTS {
        cmd("WATCH")
            .arg(key)
            .exec(&mut *connection)
            .map_err(redis_to_io)?;
        let mut pipeline = pipe();
        if let Some(result) = operation(connection, pipeline.atomic()).map_err(redis_to_io)? {
            cmd("UNWATCH").exec(&mut *connection).map_err(redis_to_io)?;
            return Ok(result);
        }
    }
    cmd("UNWATCH").exec(&mut *connection).map_err(redis_to_io)?;
    Err(Error::new(
        ErrorKind::TimedOut,
        "redis checkpoint transaction retry limit exceeded",
    ))
}

fn query_committed_transaction<C, T>(
    connection: &mut C,
    pipeline: &Pipeline,
    result: T,
) -> RedisResult<Option<T>>
where
    C: ConnectionLike,
{
    pipeline
        .query::<Option<()>>(connection)
        .map(|committed| committed.map(|()| result))
}

fn renew_checkpoint_claim_with_connection<C>(
    connection: &mut C,
    key: &str,
    claim_token: &str,
    expected_revision: u64,
    lease_expires_at_ms: u64,
    clock: &LeaseOperationClock,
) -> Result<bool>
where
    C: ConnectionLike,
{
    let Some(raw) = connection
        .get::<_, Option<String>>(key)
        .map_err(redis_to_io)?
    else {
        return Ok(false);
    };
    let mut checkpoint = RedisStateStore::checkpoint_from_json(&raw)?;
    let current_now_ms = clock.now_ms();
    if checkpoint.revision != expected_revision
        || checkpoint.claim_token.as_deref() != Some(claim_token)
        || checkpoint.lease_expires_at_ms.unwrap_or(0) <= current_now_ms
        || lease_expires_at_ms <= current_now_ms
    {
        return Ok(false);
    }
    let previous_lease_expires_at_ms = checkpoint
        .lease_expires_at_ms
        .expect("validated lease expiry must be present");
    checkpoint.lease_expires_at_ms = Some(lease_expires_at_ms);
    let payload = RedisStateStore::checkpoint_to_json(&checkpoint)?;
    let result = cmd("EVAL")
        .arg(RENEW_CLAIM_SCRIPT)
        .arg(1)
        .arg(key)
        .arg(&raw)
        .arg(payload)
        .arg(previous_lease_expires_at_ms)
        .arg(lease_expires_at_ms)
        .arg(clock.now_ms())
        .query::<i64>(connection)
        .map_err(redis_to_io)?;
    match result {
        0 => Ok(false),
        1 => Ok(true),
        2 => Err(Error::new(ErrorKind::TimedOut, "claim lease expired")),
        unexpected => Err(Error::new(
            ErrorKind::InvalidData,
            format!("redis checkpoint renewal returned unexpected result: {unexpected}"),
        )),
    }
}

impl StateStore for RedisStateStore {
    fn create_checkpoint(&self, checkpoint: Checkpoint) -> Result<bool> {
        let key = Self::checkpoint_key(&checkpoint.task_id);
        let payload = Self::checkpoint_to_json(&checkpoint)?;
        self.connection
            .lock()
            .map_err(|_| Error::other("redis state store lock is poisoned"))?
            .set_nx(key, payload)
            .map_err(redis_to_io)
    }

    fn save_checkpoint(&self, checkpoint: Checkpoint) -> Result<()> {
        let key = Self::checkpoint_key(&checkpoint.task_id);
        let payload = Self::checkpoint_to_json(&checkpoint)?;
        self.connection
            .lock()
            .map_err(|_| Error::other("redis state store lock is poisoned"))?
            .set::<_, _, ()>(key, payload)
            .map_err(redis_to_io)
    }

    fn load_checkpoint(&self, task_id: &str) -> Result<Option<Checkpoint>> {
        let key = Self::checkpoint_key(task_id);
        let raw = self
            .connection
            .lock()
            .map_err(|_| Error::other("redis state store lock is poisoned"))?
            .get::<_, Option<String>>(key)
            .map_err(redis_to_io)?;
        raw.as_deref().map(Self::checkpoint_from_json).transpose()
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
        let key = Self::checkpoint_key(task_id);
        self.transaction(&key, |connection, pipe| {
            let Some(raw) = connection.get::<_, Option<String>>(&key)? else {
                return Ok(Some(None));
            };
            let mut checkpoint = Self::checkpoint_from_json(&raw).map_err(io_to_redis)?;
            check_claim(&checkpoint, cycle_index, now_ms).map_err(io_to_redis)?;
            checkpoint.revision = checkpoint.revision.checked_add(1).ok_or_else(|| {
                io_to_redis(Error::new(
                    ErrorKind::InvalidData,
                    "checkpoint revision overflow",
                ))
            })?;
            checkpoint.claim_token = Some(claim_token.to_string());
            checkpoint.claimed_cycle = Some(cycle_index);
            checkpoint.lease_expires_at_ms = Some(lease_expires_at_ms);
            let payload = Self::checkpoint_to_json(&checkpoint).map_err(io_to_redis)?;
            pipe.set(&key, payload).ignore();
            query_committed_transaction(connection, pipe, Some(checkpoint))
        })
    }

    fn commit_checkpoint(
        &self,
        mut checkpoint: Checkpoint,
        claim_token: &str,
        expected_revision: u64,
    ) -> Result<bool> {
        let key = Self::checkpoint_key(&checkpoint.task_id);
        self.transaction(&key, |connection, pipe| {
            let current = connection.get::<_, Option<String>>(&key)?;
            let current = current
                .as_deref()
                .map(Self::checkpoint_from_json)
                .transpose()
                .map_err(io_to_redis)?;
            if !claim_matches(
                current.as_ref(),
                &checkpoint,
                claim_token,
                expected_revision,
            ) {
                return Ok(Some(false));
            }
            checkpoint.revision = expected_revision.checked_add(1).ok_or_else(|| {
                io_to_redis(Error::new(
                    ErrorKind::InvalidData,
                    "checkpoint revision overflow",
                ))
            })?;
            clear_claim(&mut checkpoint);
            let payload = Self::checkpoint_to_json(&checkpoint).map_err(io_to_redis)?;
            pipe.set(&key, payload).ignore();
            query_committed_transaction(connection, pipe, true)
        })
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
        let key = Self::checkpoint_key(task_id);
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| Error::other("redis state store lock is poisoned"))?;
        renew_checkpoint_claim_with_connection(
            &mut *connection,
            &key,
            claim_token,
            expected_revision,
            lease_expires_at_ms,
            &clock,
        )
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
        let key = Self::checkpoint_key(&checkpoint.task_id);
        self.transaction(&key, |connection, pipe| {
            let current = connection.get::<_, Option<String>>(&key)?;
            let current = current
                .as_deref()
                .map(Self::checkpoint_from_json)
                .transpose()
                .map_err(io_to_redis)?;
            if current.as_ref().is_none_or(|current| {
                current.revision != expected_revision
                    || current.claim_token.is_some()
                    || current.terminal_result.is_some()
            }) {
                return Ok(Some(false));
            }
            checkpoint.revision = expected_revision.checked_add(1).ok_or_else(|| {
                io_to_redis(Error::new(
                    ErrorKind::InvalidData,
                    "checkpoint revision overflow",
                ))
            })?;
            clear_claim(&mut checkpoint);
            let payload = Self::checkpoint_to_json(&checkpoint).map_err(io_to_redis)?;
            pipe.set(&key, payload).ignore();
            query_committed_transaction(connection, pipe, true)
        })
    }

    fn delete_checkpoint(&self, task_id: &str) -> Result<()> {
        let key = Self::checkpoint_key(task_id);
        self.connection
            .lock()
            .map_err(|_| Error::other("redis state store lock is poisoned"))?
            .del::<_, ()>(key)
            .map_err(redis_to_io)
    }

    fn acknowledge_terminal(&self, task_id: &str, expected_revision: u64) -> Result<bool> {
        let key = Self::checkpoint_key(task_id);
        self.transaction(&key, |connection, pipe| {
            let current = connection.get::<_, Option<String>>(&key)?;
            let matches = current
                .as_deref()
                .map(Self::checkpoint_from_json)
                .transpose()
                .map_err(io_to_redis)?
                .is_some_and(|checkpoint| {
                    checkpoint.revision == expected_revision && checkpoint.terminal_result.is_some()
                });
            if !matches {
                return Ok(Some(false));
            }
            pipe.del(&key).ignore();
            query_committed_transaction(connection, pipe, true)
        })
    }

    fn list_checkpoints(&self) -> Result<Vec<String>> {
        let pattern = format!("{KEY_PREFIX}*");
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| Error::other("redis state store lock is poisoned"))?;
        let iter = connection
            .scan_match::<_, String>(pattern)
            .map_err(redis_to_io)?;
        let mut keys = iter
            .map(|key| key.strip_prefix(KEY_PREFIX).unwrap_or(&key).to_string())
            .collect::<Vec<_>>();
        keys.sort();
        Ok(keys)
    }

    fn state_store_spec(&self) -> Option<StateStoreSpec> {
        StateStoreSpec::redis(&self.redis_url).ok()
    }
}

fn redis_to_io(error: redis::RedisError) -> Error {
    Error::other(error.to_string())
}

fn io_to_redis(error: Error) -> redis::RedisError {
    redis::RedisError::from((
        redis::ErrorKind::TypeError,
        "checkpoint state error",
        error.to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct ScriptedConnection {
        exec_responses: VecDeque<redis::Value>,
        watch_calls: usize,
        unwatch_calls: usize,
        exec_calls: usize,
    }

    impl ScriptedConnection {
        fn new(exec_responses: impl IntoIterator<Item = redis::Value>) -> Self {
            Self {
                exec_responses: exec_responses.into_iter().collect(),
                watch_calls: 0,
                unwatch_calls: 0,
                exec_calls: 0,
            }
        }
    }

    impl ConnectionLike for ScriptedConnection {
        fn req_packed_command(&mut self, command: &[u8]) -> RedisResult<redis::Value> {
            if command
                .windows(b"UNWATCH".len())
                .any(|window| window == b"UNWATCH")
            {
                self.unwatch_calls += 1;
            } else if command
                .windows(b"WATCH".len())
                .any(|window| window == b"WATCH")
            {
                self.watch_calls += 1;
            } else {
                panic!("unexpected standalone Redis command");
            }
            Ok(redis::Value::Okay)
        }

        fn req_packed_commands(
            &mut self,
            _command: &[u8],
            _offset: usize,
            _count: usize,
        ) -> RedisResult<Vec<redis::Value>> {
            self.exec_calls += 1;
            let response = self
                .exec_responses
                .pop_front()
                .expect("scripted EXEC response");
            Ok(vec![response])
        }

        fn get_db(&self) -> i64 {
            0
        }

        fn check_connection(&mut self) -> bool {
            true
        }

        fn is_open(&self) -> bool {
            true
        }
    }

    struct AtomicRenewConnection {
        value: Option<String>,
        replace_before_eval: Option<String>,
        server_now_ms: u64,
        get_calls: usize,
        eval_calls: usize,
    }

    impl AtomicRenewConnection {
        fn new(value: String, server_now_ms: u64) -> Self {
            Self {
                value: Some(value),
                replace_before_eval: None,
                server_now_ms,
                get_calls: 0,
                eval_calls: 0,
            }
        }

        fn replace_before_eval(mut self, value: String) -> Self {
            self.replace_before_eval = Some(value);
            self
        }
    }

    impl ConnectionLike for AtomicRenewConnection {
        fn req_packed_command(&mut self, command: &[u8]) -> RedisResult<redis::Value> {
            let arguments = packed_command_arguments(command);
            match arguments.first().map(Vec::as_slice) {
                Some(b"GET") => {
                    self.get_calls += 1;
                    Ok(self
                        .value
                        .as_ref()
                        .map(|value| redis::Value::BulkString(value.as_bytes().to_vec()))
                        .unwrap_or(redis::Value::Nil))
                }
                Some(b"EVAL") => {
                    self.eval_calls += 1;
                    assert_eq!(arguments[1], RENEW_CLAIM_SCRIPT.as_bytes());
                    assert_eq!(arguments[2], b"1");
                    if let Some(replacement) = self.replace_before_eval.take() {
                        self.value = Some(replacement);
                    }
                    let expected = std::str::from_utf8(&arguments[4]).expect("expected payload");
                    let updated = std::str::from_utf8(&arguments[5]).expect("updated payload");
                    let previous_expiry = parse_u64_argument(&arguments[6]);
                    let requested_expiry = parse_u64_argument(&arguments[7]);
                    let client_now = parse_u64_argument(&arguments[8]);
                    let current_now = self.server_now_ms.max(client_now);
                    if previous_expiry <= current_now || requested_expiry <= current_now {
                        return Ok(redis::Value::Int(2));
                    }
                    if self.value.as_deref() != Some(expected) {
                        return Ok(redis::Value::Int(0));
                    }
                    self.value = Some(updated.to_string());
                    Ok(redis::Value::Int(1))
                }
                command => panic!("unexpected standalone Redis command: {command:?}"),
            }
        }

        fn req_packed_commands(
            &mut self,
            _command: &[u8],
            _offset: usize,
            _count: usize,
        ) -> RedisResult<Vec<redis::Value>> {
            panic!("atomic renewal does not use a pipeline")
        }

        fn get_db(&self) -> i64 {
            0
        }

        fn check_connection(&mut self) -> bool {
            true
        }

        fn is_open(&self) -> bool {
            true
        }
    }

    fn packed_command_arguments(command: &[u8]) -> Vec<Vec<u8>> {
        let mut cursor = 0;
        assert_eq!(command.get(cursor), Some(&b'*'));
        cursor += 1;
        let count = read_resp_number(command, &mut cursor);
        (0..count)
            .map(|_| {
                assert_eq!(command.get(cursor), Some(&b'$'));
                cursor += 1;
                let length = read_resp_number(command, &mut cursor);
                let value = command[cursor..cursor + length].to_vec();
                cursor += length;
                assert_eq!(&command[cursor..cursor + 2], b"\r\n");
                cursor += 2;
                value
            })
            .collect()
    }

    fn read_resp_number(command: &[u8], cursor: &mut usize) -> usize {
        let line_start = *cursor;
        while &command[*cursor..*cursor + 2] != b"\r\n" {
            *cursor += 1;
        }
        let number = std::str::from_utf8(&command[line_start..*cursor])
            .expect("RESP number")
            .parse()
            .expect("valid RESP number");
        *cursor += 2;
        number
    }

    fn parse_u64_argument(value: &[u8]) -> u64 {
        std::str::from_utf8(value)
            .expect("integer argument")
            .parse()
            .expect("valid u64 argument")
    }

    #[test]
    fn exec_nil_retries_until_a_transaction_commits() {
        let mut connection = ScriptedConnection::new([
            redis::Value::Nil,
            redis::Value::Nil,
            redis::Value::Array(vec![redis::Value::Okay]),
        ]);
        let mut operation_calls = 0;

        let committed =
            transaction_with_connection(&mut connection, "checkpoint", |connection, pipe| {
                operation_calls += 1;
                pipe.set("checkpoint", "value").ignore();
                query_committed_transaction(connection, pipe, true)
            })
            .expect("third transaction attempt commits");

        assert!(committed);
        assert_eq!(operation_calls, 3);
        assert_eq!(connection.watch_calls, 3);
        assert_eq!(connection.exec_calls, 3);
        assert_eq!(connection.unwatch_calls, 1);
    }

    #[test]
    fn exec_nil_stops_after_the_transaction_retry_limit() {
        let mut connection = ScriptedConnection::new(std::iter::repeat_n(
            redis::Value::Nil,
            TRANSACTION_MAX_ATTEMPTS,
        ));
        let mut operation_calls = 0;

        let error =
            transaction_with_connection(&mut connection, "checkpoint", |connection, pipe| {
                operation_calls += 1;
                pipe.set("checkpoint", "value").ignore();
                query_committed_transaction(connection, pipe, true)
            })
            .expect_err("transaction conflicts must be bounded");

        assert_eq!(error.kind(), ErrorKind::TimedOut);
        assert_eq!(
            error.to_string(),
            "redis checkpoint transaction retry limit exceeded"
        );
        assert_eq!(operation_calls, TRANSACTION_MAX_ATTEMPTS);
        assert_eq!(connection.watch_calls, TRANSACTION_MAX_ATTEMPTS);
        assert_eq!(connection.exec_calls, TRANSACTION_MAX_ATTEMPTS);
        assert_eq!(connection.unwatch_calls, 1);
    }

    #[test]
    fn renewal_atomic_script_updates_an_active_owner() {
        let checkpoint = renewal_checkpoint("renewal-active", 200);
        let raw = RedisStateStore::checkpoint_to_json(&checkpoint).expect("checkpoint json");
        let mut connection = AtomicRenewConnection::new(raw, 150);
        let clock = LeaseOperationClock::new(150);

        let renewed = renew_checkpoint_claim_with_connection(
            &mut connection,
            "checkpoint",
            "owner",
            1,
            300,
            &clock,
        )
        .expect("renewal outcome");

        assert!(renewed);
        let persisted = RedisStateStore::checkpoint_from_json(
            connection.value.as_deref().expect("persisted checkpoint"),
        )
        .expect("persisted checkpoint json");
        assert_eq!(persisted.lease_expires_at_ms, Some(300));
        assert_eq!(connection.get_calls, 1);
        assert_eq!(connection.eval_calls, 1);
    }

    #[test]
    fn renewal_atomic_script_rejects_owner_expired_at_write() {
        let checkpoint = renewal_checkpoint("renewal-expired-at-write", 110);
        let raw = RedisStateStore::checkpoint_to_json(&checkpoint).expect("checkpoint json");
        let mut connection = AtomicRenewConnection::new(raw.clone(), 110);
        let clock = LeaseOperationClock::new(100);

        let error = renew_checkpoint_claim_with_connection(
            &mut connection,
            "checkpoint",
            "owner",
            1,
            1_000,
            &clock,
        )
        .expect_err("an owner expired at the atomic write boundary must fail explicitly");

        assert_eq!(error.kind(), ErrorKind::TimedOut);
        assert_eq!(error.to_string(), "claim lease expired");
        assert_eq!(connection.value.as_deref(), Some(raw.as_str()));
        let persisted = RedisStateStore::checkpoint_from_json(&raw).expect("checkpoint json");
        check_claim(&persisted, 1, 110).expect("an expired claim is immediately reclaimable");
        assert_eq!(connection.get_calls, 1);
        assert_eq!(connection.eval_calls, 1);
    }

    #[test]
    fn renewal_atomic_script_does_not_overwrite_a_new_owner() {
        let checkpoint = renewal_checkpoint("renewal-cas-mismatch", 200);
        let replacement = Checkpoint {
            revision: 2,
            claim_token: Some("contender".to_string()),
            lease_expires_at_ms: Some(500),
            ..checkpoint.clone()
        };
        let raw = RedisStateStore::checkpoint_to_json(&checkpoint).expect("checkpoint json");
        let replacement_raw =
            RedisStateStore::checkpoint_to_json(&replacement).expect("replacement checkpoint json");
        let mut connection =
            AtomicRenewConnection::new(raw, 150).replace_before_eval(replacement_raw.clone());
        let clock = LeaseOperationClock::new(150);

        let renewed = renew_checkpoint_claim_with_connection(
            &mut connection,
            "checkpoint",
            "owner",
            1,
            300,
            &clock,
        )
        .expect("renewal outcome");

        assert!(!renewed);
        assert_eq!(connection.value.as_deref(), Some(replacement_raw.as_str()));
    }

    #[test]
    fn renewal_atomic_script_prioritizes_expiry_over_cas_mismatch() {
        let checkpoint = renewal_checkpoint("renewal-expiry-before-cas", 200);
        let replacement = Checkpoint {
            revision: 2,
            claim_token: None,
            claimed_cycle: None,
            lease_expires_at_ms: None,
            ..checkpoint.clone()
        };
        let raw = RedisStateStore::checkpoint_to_json(&checkpoint).expect("checkpoint json");
        let replacement_raw =
            RedisStateStore::checkpoint_to_json(&replacement).expect("replacement checkpoint json");
        let mut connection =
            AtomicRenewConnection::new(raw, 200).replace_before_eval(replacement_raw.clone());
        let clock = LeaseOperationClock::new(150);

        let error = renew_checkpoint_claim_with_connection(
            &mut connection,
            "checkpoint",
            "owner",
            1,
            300,
            &clock,
        )
        .expect_err("authoritative expiry must take precedence over CAS loss");

        assert_eq!(error.kind(), ErrorKind::TimedOut);
        assert_eq!(error.to_string(), "claim lease expired");
        assert_eq!(connection.value.as_deref(), Some(replacement_raw.as_str()));
    }

    fn renewal_checkpoint(task_id: &str, lease_expires_at_ms: u64) -> Checkpoint {
        Checkpoint {
            task_id: task_id.to_string(),
            cycle_index: 0,
            status: crate::types::AgentStatus::Running,
            messages: Vec::new(),
            cycles: Vec::new(),
            shared_state: Default::default(),
            revision: 1,
            claim_token: Some("owner".to_string()),
            claimed_cycle: Some(1),
            lease_expires_at_ms: Some(lease_expires_at_ms),
            terminal_result: None,
        }
    }
}
