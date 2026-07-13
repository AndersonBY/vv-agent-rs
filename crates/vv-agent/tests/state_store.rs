use serde_json::json;
use tempfile::NamedTempFile;
use vv_agent::runtime::state::{Checkpoint, InMemoryStateStore, StateStore};
use vv_agent::runtime::stores::redis::RedisStateStore;
use vv_agent::runtime::stores::sqlite::SqliteStateStore;
use vv_agent::{AgentStatus, CycleRecord, Message, ToolCall, ToolExecutionResult};

fn checkpoint(task_id: &str, cycle_index: u32) -> Checkpoint {
    let mut assistant = Message::assistant("hi there");
    assistant.tool_calls = vec![ToolCall::new(
        "c1",
        "test",
        [("key".to_string(), json!("val"))].into_iter().collect(),
    )];
    Checkpoint {
        task_id: task_id.to_string(),
        cycle_index,
        status: AgentStatus::Running,
        messages: vec![
            Message::system("sys prompt"),
            Message::user("hello"),
            assistant,
        ],
        cycles: vec![CycleRecord {
            index: 1,
            assistant_message: "hi there".to_string(),
            tool_calls: vec![ToolCall::new(
                "c1",
                "test",
                [("key".to_string(), json!("val"))].into_iter().collect(),
            )],
            tool_results: vec![ToolExecutionResult::success("c1", "result")],
            memory_compacted: false,
            token_usage: Default::default(),
        }],
        shared_state: [
            ("todo_list".to_string(), json!([])),
            ("counter".to_string(), json!(42)),
        ]
        .into_iter()
        .collect(),
        revision: 0,
        claim_token: None,
        claimed_cycle: None,
        lease_expires_at_ms: None,
        terminal_result: None,
    }
}

#[test]
fn sqlite_state_store_persists_agent_to_dict_payload_shape() {
    let db = NamedTempFile::new().expect("temp sqlite db");
    let store = SqliteStateStore::new(db.path()).expect("sqlite store");

    store
        .save_checkpoint(checkpoint("task-shape", 3))
        .expect("save");
    store.close().expect("close");

    let connection = rusqlite::Connection::open(db.path()).expect("open sqlite db");
    let (messages, cycles): (String, String) = connection
        .query_row(
            "SELECT messages, cycles FROM checkpoints WHERE task_id = ?1",
            ["task-shape"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("checkpoint row");
    let messages: serde_json::Value = serde_json::from_str(&messages).expect("messages json");
    let cycles: serde_json::Value = serde_json::from_str(&cycles).expect("cycles json");

    assert_eq!(
        messages[2]["tool_calls"][0],
        json!({
            "id": "c1",
            "type": "function",
            "function": {
                "name": "test",
                "arguments": "{\"key\":\"val\"}"
            }
        })
    );
    assert_eq!(
        cycles[0]["tool_calls"][0],
        json!({
            "id": "c1",
            "name": "test",
            "arguments": {"key": "val"}
        })
    );
}

#[test]
fn in_memory_state_store_saves_loads_lists_deletes_and_overwrites() {
    let store = InMemoryStateStore::default();

    store
        .save_checkpoint(checkpoint("task-1", 3))
        .expect("save");
    let loaded = store
        .load_checkpoint("task-1")
        .expect("load")
        .expect("exists");
    assert_eq!(loaded.task_id, "task-1");
    assert_eq!(loaded.cycle_index, 3);
    assert_eq!(loaded.messages.len(), 3);
    assert_eq!(loaded.cycles[0].tool_calls[0].name, "test");

    store
        .save_checkpoint(checkpoint("task-1", 5))
        .expect("overwrite");
    store
        .save_checkpoint(checkpoint("task-2", 1))
        .expect("save 2");
    assert_eq!(
        store.list_checkpoints().expect("list"),
        vec!["task-1", "task-2"]
    );
    assert_eq!(
        store
            .load_checkpoint("task-1")
            .expect("load overwritten")
            .expect("exists")
            .cycle_index,
        5
    );

    store.delete_checkpoint("task-1").expect("delete");
    assert!(store
        .load_checkpoint("task-1")
        .expect("load missing")
        .is_none());
}

#[test]
fn sqlite_state_store_round_trips_checkpoints() {
    let store = SqliteStateStore::new(":memory:").expect("sqlite store");

    store
        .save_checkpoint(checkpoint("task-1", 3))
        .expect("save");
    let loaded = store
        .load_checkpoint("task-1")
        .expect("load")
        .expect("exists");
    assert_eq!(loaded.status, AgentStatus::Running);
    assert_eq!(loaded.messages[0].content, "sys prompt");
    assert_eq!(loaded.cycles[0].tool_results[0].content, "result");
    assert_eq!(loaded.shared_state["counter"], json!(42));

    store
        .save_checkpoint(checkpoint("task-2", 1))
        .expect("save 2");
    assert_eq!(
        store.list_checkpoints().expect("list"),
        vec!["task-1", "task-2"]
    );

    store.delete_checkpoint("task-1").expect("delete");
    assert!(store.load_checkpoint("task-1").expect("missing").is_none());
    store.close().expect("close");
}

#[test]
fn redis_state_store_matches_key_and_payload_shape() {
    let checkpoint = checkpoint("task-redis", 7);
    let payload = RedisStateStore::checkpoint_to_json(&checkpoint).expect("payload");
    let parsed: serde_json::Value = serde_json::from_str(&payload).expect("json payload");

    assert_eq!(
        RedisStateStore::checkpoint_key("task-redis"),
        "vv_agent:checkpoint:task-redis"
    );
    assert_eq!(parsed["task_id"], "task-redis");
    assert_eq!(parsed["cycle_index"], 7);
    assert_eq!(parsed["status"], "running");
    assert!(parsed["messages"].is_array());
    assert!(parsed["cycles"].is_array());
    assert_eq!(
        parsed["messages"][2]["tool_calls"][0],
        json!({
            "id": "c1",
            "type": "function",
            "function": {
                "name": "test",
                "arguments": "{\"key\":\"val\"}"
            }
        })
    );
    assert_eq!(
        parsed["cycles"][0]["tool_calls"][0],
        json!({
            "id": "c1",
            "name": "test",
            "arguments": {"key": "val"}
        })
    );
    assert_eq!(parsed["shared_state"]["counter"], json!(42));

    let round_trip = RedisStateStore::checkpoint_from_json(&payload).expect("round trip");
    assert_eq!(round_trip, checkpoint);
}

#[test]
#[ignore = "requires VV_AGENT_REDIS_URL and a live Redis instance"]
fn redis_state_store_round_trips_checkpoints_against_live_redis() {
    let redis_url = std::env::var("VV_AGENT_REDIS_URL").expect("VV_AGENT_REDIS_URL");
    let store = RedisStateStore::new(redis_url).expect("redis store");
    store.delete_checkpoint("task-live-redis").expect("cleanup");

    store
        .save_checkpoint(checkpoint("task-live-redis", 4))
        .expect("save");
    let loaded = store
        .load_checkpoint("task-live-redis")
        .expect("load")
        .expect("exists");
    assert_eq!(loaded.cycle_index, 4);
    assert!(store
        .list_checkpoints()
        .expect("list")
        .contains(&"task-live-redis".to_string()));

    store.delete_checkpoint("task-live-redis").expect("delete");
    assert!(store
        .load_checkpoint("task-live-redis")
        .expect("missing")
        .is_none());
}
