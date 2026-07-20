use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use rusqlite::Connection;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use vv_agent::runtime::state::{Checkpoint, InMemoryStateStore, StateStore};
use vv_agent::runtime::stores::redis::RedisStateStore;
use vv_agent::runtime::stores::sqlite::SqliteStateStore;
use vv_agent::{AgentResult, AgentStatus, Message};

const FIXTURE: &str = include_str!("fixtures/parity/checkpoint_codec_v1.json");
const FIXTURE_SHA256: &str = "e7be2cfafca7f741d32b4537cb003f0179f69162171432c17cd746a0ff2119cf";

fn checkpoint(task_id: &str) -> Checkpoint {
    Checkpoint {
        task_id: task_id.to_string(),
        cycle_index: 0,
        status: AgentStatus::Running,
        messages: vec![Message::user("hello")],
        cycles: Vec::new(),
        shared_state: [("nested".to_string(), json!({"value": 1}))]
            .into_iter()
            .collect(),
        revision: 0,
        claim_token: None,
        claimed_cycle: None,
        lease_expires_at_ms: None,
        terminal_result: None,
        budget_usage: None,
    }
}

fn completed_result(checkpoint: &Checkpoint, answer: &str) -> AgentResult {
    AgentResult::completed_with_shared_state(
        checkpoint.messages.clone(),
        checkpoint.cycles.clone(),
        answer,
        checkpoint.shared_state.clone(),
    )
}

fn assert_terminal_finalize_is_immutable(store: &dyn StateStore, task_id: &str) {
    assert!(store.create_checkpoint(checkpoint(task_id)).unwrap());
    let mut first = store.load_checkpoint(task_id).unwrap().expect("checkpoint");
    let first_result = completed_result(&first, "first");
    first.status = first_result.status;
    first.terminal_result = Some(first_result.clone());
    let first_revision = first.revision;
    assert!(store.finalize_checkpoint(first, first_revision).unwrap());

    let persisted = store
        .load_checkpoint(task_id)
        .unwrap()
        .expect("terminal checkpoint");
    let mut replacement = persisted.clone();
    replacement.terminal_result = Some(completed_result(&replacement, "replacement"));
    assert!(!store
        .finalize_checkpoint(replacement, persisted.revision)
        .unwrap());

    let unchanged = store
        .load_checkpoint(task_id)
        .unwrap()
        .expect("unchanged terminal checkpoint");
    assert_eq!(unchanged.revision, persisted.revision);
    assert_eq!(unchanged.terminal_result, Some(first_result));
}

#[test]
fn shared_checkpoint_fixture_hash_and_codec_corpus_match() {
    let digest = Sha256::digest(FIXTURE.as_bytes());
    let digest = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert_eq!(digest, FIXTURE_SHA256);
    let fixture: Value = serde_json::from_str(FIXTURE).expect("fixture json");
    for payload in std::iter::once(&fixture["canonical"]).chain(
        fixture["valid_cases"]
            .as_array()
            .expect("valid cases")
            .iter()
            .map(|case| &case["payload"]),
    ) {
        let raw = serde_json::to_string(payload).expect("payload json");
        let decoded = RedisStateStore::checkpoint_from_json(&raw).expect("valid checkpoint");
        let encoded = RedisStateStore::checkpoint_to_json(&decoded).expect("encoded checkpoint");
        assert_eq!(serde_json::from_str::<Value>(&encoded).unwrap(), *payload);
    }
    for case in fixture["invalid_cases"].as_array().expect("invalid cases") {
        let raw = serde_json::to_string(&case["payload"]).expect("invalid payload json");
        assert!(
            RedisStateStore::checkpoint_from_json(&raw).is_err(),
            "{}",
            case["name"]
        );
    }
}

#[test]
fn memory_and_sqlite_revision_leases_reject_stale_commits_and_persist_terminal() {
    let directory = TempDir::new().expect("temp directory");
    let stores: Vec<Box<dyn StateStore>> = vec![
        Box::new(InMemoryStateStore::new()),
        Box::new(SqliteStateStore::new(directory.path().join("checkpoints.sqlite3")).unwrap()),
    ];

    for (index, store) in stores.into_iter().enumerate() {
        let task_id = format!("claim-{index}");
        assert!(store.create_checkpoint(checkpoint(&task_id)).unwrap());
        assert!(!store.create_checkpoint(checkpoint(&task_id)).unwrap());

        let first = store
            .claim_checkpoint(&task_id, 1, "first", 200, 100)
            .unwrap()
            .expect("first claim");
        assert!(store
            .renew_checkpoint_claim(&task_id, "first", first.revision, 300, 150)
            .unwrap());
        assert!(!store
            .renew_checkpoint_claim(&task_id, "wrong", first.revision, 320, 160)
            .unwrap());
        assert_eq!(
            store
                .load_checkpoint(&task_id)
                .unwrap()
                .expect("renewed claim")
                .lease_expires_at_ms,
            Some(300)
        );
        assert!(store
            .claim_checkpoint(&task_id, 1, "duplicate", 250, 150)
            .is_err());
        let mut retry = store
            .claim_checkpoint(&task_id, 1, "retry", 400, 300)
            .unwrap()
            .expect("expired lease retry");
        retry.cycle_index = 1;
        assert!(!store
            .commit_checkpoint(retry.clone(), "first", first.revision)
            .unwrap());
        let retry_revision = retry.revision;
        assert!(store
            .commit_checkpoint(retry, "retry", retry_revision)
            .unwrap());

        let mut committed = store.load_checkpoint(&task_id).unwrap().expect("committed");
        let result = AgentResult {
            status: AgentStatus::MaxCycles,
            messages: committed.messages.clone(),
            cycles: committed.cycles.clone(),
            completion_reason: Some(vv_agent::CompletionReason::MaxCycles),
            completion_tool_name: None,
            partial_output: None,
            final_answer: Some("done".to_string()),
            wait_reason: None,
            error: None,
            error_code: None,
            shared_state: committed.shared_state.clone(),
            token_usage: Default::default(),
            budget_usage: None,
            budget_exhaustion: None,
            checkpoint_key: None,
            resume_observation: None,
        };
        let revision = committed.revision;
        committed.status = result.status;
        committed.terminal_result = Some(result);
        assert!(store.finalize_checkpoint(committed, revision).unwrap());
        let terminal = store.load_checkpoint(&task_id).unwrap().expect("terminal");
        assert!(terminal.terminal_result.is_some());
        assert!(!store
            .acknowledge_terminal(&task_id, terminal.revision - 1)
            .unwrap());
        assert!(store
            .acknowledge_terminal(&task_id, terminal.revision)
            .unwrap());
    }
}

#[test]
fn memory_and_sqlite_finalize_never_overwrite_a_terminal_checkpoint() {
    let directory = TempDir::new().expect("temp directory");
    let stores: Vec<Box<dyn StateStore>> = vec![
        Box::new(InMemoryStateStore::new()),
        Box::new(SqliteStateStore::new(directory.path().join("immutable.sqlite3")).unwrap()),
    ];

    for (index, store) in stores.into_iter().enumerate() {
        assert_terminal_finalize_is_immutable(store.as_ref(), &format!("immutable-{index}"));
    }
}

#[test]
fn redis_finalize_never_overwrites_a_terminal_checkpoint_when_configured() {
    let Ok(redis_url) = std::env::var("VV_AGENT_REDIS_URL") else {
        return;
    };
    let store = RedisStateStore::new(redis_url).expect("redis store");
    let task_id = format!("immutable-live-redis-{}", std::process::id());
    store.delete_checkpoint(&task_id).expect("redis cleanup");
    assert_terminal_finalize_is_immutable(&store, &task_id);
    store.delete_checkpoint(&task_id).expect("redis cleanup");
}

#[test]
fn sqlite_store_binds_relative_path_at_construction() {
    let directory = TempDir::new().expect("temp directory");
    let original = std::env::current_dir().expect("cwd");
    std::env::set_current_dir(directory.path()).expect("enter temp directory");
    let store = SqliteStateStore::new("state.sqlite3").expect("relative sqlite store");
    let spec = store.state_store_spec().expect("state store spec");
    std::env::set_current_dir(&original).expect("restore cwd");

    assert_eq!(
        spec.location,
        directory.path().join("state.sqlite3").to_string_lossy()
    );
    let rebuilt = spec.build().expect("rebuilt store");
    assert!(store.create_checkpoint(checkpoint("bound-path")).unwrap());
    assert!(rebuilt.load_checkpoint("bound-path").unwrap().is_some());
}

#[test]
fn sqlite_migrates_legacy_checkpoint_table_in_place() {
    let directory = TempDir::new().expect("temp directory");
    let path = directory.path().join("legacy.sqlite3");
    let legacy = Connection::open(&path).expect("legacy sqlite connection");
    legacy
        .execute_batch(
            r#"
            CREATE TABLE checkpoints (
                task_id TEXT PRIMARY KEY,
                cycle_index INTEGER NOT NULL,
                status TEXT NOT NULL,
                messages TEXT NOT NULL,
                cycles TEXT NOT NULL,
                shared_state TEXT NOT NULL
            );
            INSERT INTO checkpoints VALUES ('legacy-task', 0, 'running', '[]', '[]', '{}');
            "#,
        )
        .expect("legacy schema");
    drop(legacy);

    let store = SqliteStateStore::new(&path).expect("migrated store");
    let checkpoint = store
        .load_checkpoint("legacy-task")
        .expect("load migrated checkpoint")
        .expect("migrated checkpoint");

    assert_eq!(checkpoint.revision, 0);
    assert!(checkpoint.claim_token.is_none());
    assert!(checkpoint.terminal_result.is_none());
    let claimed = store
        .claim_checkpoint("legacy-task", 1, "migrated-worker", 200, 100)
        .expect("claim migrated checkpoint")
        .expect("migrated checkpoint claim");
    assert_eq!(claimed.revision, 1);
}

#[test]
fn sqlite_second_connection_waits_for_short_write_contention() {
    let directory = TempDir::new().expect("temp directory");
    let path = directory.path().join("contention.sqlite3");
    let store = Arc::new(SqliteStateStore::new(&path).expect("state store"));
    let locker = Connection::open(&path).expect("locker connection");
    locker
        .busy_timeout(Duration::from_secs(5))
        .expect("locker busy timeout");
    locker
        .execute_batch("BEGIN IMMEDIATE")
        .expect("acquire write lock");
    let worker_store = store.clone();
    let worker =
        thread::spawn(move || worker_store.create_checkpoint(checkpoint("contended-task")));

    thread::sleep(Duration::from_millis(100));
    assert!(
        !worker.is_finished(),
        "the second connection should wait instead of failing immediately"
    );
    locker.execute_batch("COMMIT").expect("release write lock");
    assert!(worker
        .join()
        .expect("contention worker")
        .expect("create checkpoint"));
    assert!(store
        .load_checkpoint("contended-task")
        .expect("load contended checkpoint")
        .is_some());
}

#[test]
fn sqlite_renewal_refreshes_time_after_write_lock_wait() {
    let directory = TempDir::new().expect("temp directory");
    let path = directory.path().join("renewal-contention.sqlite3");
    let store = Arc::new(SqliteStateStore::new(&path).expect("state store"));
    let task_id = "contended-renewal";
    assert!(store.create_checkpoint(checkpoint(task_id)).unwrap());
    let claimed = store
        .claim_checkpoint(task_id, 1, "owner", 150, 100)
        .expect("claim result")
        .expect("claimed checkpoint");
    let locker = Connection::open(&path).expect("locker connection");
    locker
        .busy_timeout(Duration::from_secs(5))
        .expect("locker busy timeout");
    locker
        .execute_batch("BEGIN IMMEDIATE")
        .expect("acquire write lock");
    let worker_store = store.clone();
    let worker = thread::spawn(move || {
        worker_store.renew_checkpoint_claim(task_id, "owner", claimed.revision, 300, 100)
    });

    thread::sleep(Duration::from_millis(80));
    assert!(
        !worker.is_finished(),
        "renewal should wait for the SQLite writer lock"
    );
    locker.execute_batch("COMMIT").expect("release write lock");
    assert!(!worker
        .join()
        .expect("renewal worker")
        .expect("renewal outcome"));
    assert_eq!(
        store
            .load_checkpoint(task_id)
            .expect("load checkpoint")
            .expect("persisted checkpoint")
            .lease_expires_at_ms,
        Some(150)
    );
}

#[test]
fn claimed_terminal_result_commits_before_scheduler_acknowledgement() {
    let store = InMemoryStateStore::new();
    let task_id = "terminal-claim";
    assert!(store.create_checkpoint(checkpoint(task_id)).unwrap());
    let mut claimed = store
        .claim_checkpoint(task_id, 1, "terminal", 200, 100)
        .unwrap()
        .expect("claim");
    claimed.cycle_index = 1;
    claimed.status = AgentStatus::Completed;
    claimed.terminal_result = Some(AgentResult {
        status: AgentStatus::Completed,
        messages: claimed.messages.clone(),
        cycles: claimed.cycles.clone(),
        completion_reason: Some(vv_agent::CompletionReason::ToolFinish),
        completion_tool_name: Some("task_finish".to_string()),
        partial_output: None,
        final_answer: Some("done".to_string()),
        wait_reason: None,
        error: None,
        error_code: None,
        shared_state: claimed.shared_state.clone(),
        token_usage: Default::default(),
        budget_usage: None,
        budget_exhaustion: None,
        checkpoint_key: None,
        resume_observation: None,
    });
    let revision = claimed.revision;

    assert!(store
        .commit_checkpoint(claimed, "terminal", revision)
        .unwrap());
    let persisted = store.load_checkpoint(task_id).unwrap().expect("persisted");
    assert!(persisted.claim_token.is_none());
    assert_eq!(
        persisted
            .terminal_result
            .as_ref()
            .and_then(|result| result.final_answer.as_deref()),
        Some("done")
    );
    assert!(store.acknowledge_terminal(task_id, revision + 1).unwrap());
}

#[test]
fn python_and_rust_fixture_copies_are_byte_identical_when_workspace_is_present() {
    let rust_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/parity/checkpoint_codec_v1.json");
    let python_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../vv-agent/tests/fixtures/parity/checkpoint_codec_v1.json");
    let rust_lock = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contract.lock.json");
    let python_lock =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../vv-agent/contract.lock.json");
    let locks_match = [rust_lock, python_lock]
        .map(fs::read_to_string)
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .ok()
        .and_then(|locks| {
            locks
                .into_iter()
                .map(|lock| serde_json::from_str::<Value>(&lock).ok())
                .collect::<Option<Vec<_>>>()
        })
        .is_some_and(|locks| {
            locks[0]["contract_version"] == locks[1]["contract_version"]
                && locks[0]["contract_revision"] == locks[1]["contract_revision"]
        });
    if python_path.exists() && locks_match {
        assert_eq!(fs::read(rust_path).unwrap(), fs::read(python_path).unwrap());
    }
}

#[test]
fn cross_runtime_sqlite_probe_from_environment() {
    let Ok(path) = std::env::var("VV_AGENT_CROSS_RUNTIME_DB") else {
        return;
    };
    let mode =
        std::env::var("VV_AGENT_CROSS_RUNTIME_MODE").unwrap_or_else(|_| "read_python".to_string());
    let store = SqliteStateStore::new(path).expect("cross-runtime sqlite store");
    match mode.as_str() {
        "read_python" => {
            let checkpoint = store
                .load_checkpoint("python-wrote")
                .expect("load Python checkpoint")
                .expect("Python checkpoint exists");
            assert_eq!(checkpoint.revision, 7);
            assert_eq!(checkpoint.messages[0].content, "from Python");
            assert_eq!(checkpoint.shared_state["writer"], json!("python"));
        }
        "write_rust" => {
            let mut checkpoint = checkpoint("rust-wrote");
            checkpoint.revision = 9;
            checkpoint.messages = vec![Message::user("from Rust")];
            checkpoint
                .shared_state
                .insert("writer".to_string(), json!("rust"));
            store
                .save_checkpoint(checkpoint)
                .expect("write Rust checkpoint");
        }
        other => panic!("unknown cross-runtime mode: {other}"),
    }
}
