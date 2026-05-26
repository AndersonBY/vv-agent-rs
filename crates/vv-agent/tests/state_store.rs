use serde_json::json;
use vv_agent::runtime::state::{Checkpoint, InMemoryStateStore, StateStore};
use vv_agent::runtime::stores::sqlite::SqliteStateStore;
use vv_agent::{AgentStatus, CycleRecord, Message, ToolCall, ToolExecutionResult};

fn checkpoint(task_id: &str, cycle_index: u32) -> Checkpoint {
    Checkpoint {
        task_id: task_id.to_string(),
        cycle_index,
        status: AgentStatus::Running,
        messages: vec![
            Message::system("sys prompt"),
            Message::user("hello"),
            Message::assistant("hi there"),
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
    }
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
