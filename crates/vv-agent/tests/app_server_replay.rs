use vv_agent::app_server::protocol::{AppItem, AppItemKind, AppItemStatus, ThreadStartParams};
use vv_agent::app_server::thread_store::SqliteThreadStore;

#[test]
fn thread_store_creates_and_reads_thread() {
    let store = SqliteThreadStore::in_memory().expect("store");
    let thread = store
        .create_thread(ThreadStartParams {
            cwd: Some("/tmp/project".into()),
            title: Some("Investigate".to_string()),
            model: Some("demo-model".to_string()),
            ephemeral: false,
        })
        .expect("create thread");

    let loaded = store
        .get_thread(&thread.id)
        .expect("read thread")
        .expect("thread exists");

    assert_eq!(loaded.id, thread.id);
    assert_eq!(loaded.title.as_deref(), Some("Investigate"));
    assert_eq!(loaded.model.as_deref(), Some("demo-model"));
    assert!(!loaded.ephemeral);
}

#[test]
fn thread_store_lists_non_archived_threads_newest_first() {
    let store = SqliteThreadStore::in_memory().expect("store");
    let first = store
        .create_thread(ThreadStartParams {
            cwd: None,
            title: Some("first".to_string()),
            model: None,
            ephemeral: false,
        })
        .expect("first");
    let second = store
        .create_thread(ThreadStartParams {
            cwd: None,
            title: Some("second".to_string()),
            model: None,
            ephemeral: false,
        })
        .expect("second");

    let threads = store.list_threads(false).expect("list");

    assert_eq!(threads[0].id, second.id);
    assert_eq!(threads[1].id, first.id);
}

#[test]
fn thread_store_archive_hides_thread_from_default_list() {
    let store = SqliteThreadStore::in_memory().expect("store");
    let thread = store
        .create_thread(ThreadStartParams {
            cwd: None,
            title: Some("archive me".to_string()),
            model: None,
            ephemeral: false,
        })
        .expect("create");

    store.archive_thread(&thread.id).expect("archive");

    assert!(store.list_threads(false).expect("active").is_empty());
    let archived = store.list_threads(true).expect("all");
    assert_eq!(archived.len(), 1);
    assert!(archived[0].archived);
}

#[test]
fn thread_store_appends_and_replays_items_in_insert_order() {
    let store = SqliteThreadStore::in_memory().expect("store");
    let thread = store
        .create_thread(ThreadStartParams {
            cwd: None,
            title: None,
            model: None,
            ephemeral: false,
        })
        .expect("create");

    for index in 0..5 {
        store
            .append_item(&thread.id, "turn_1", test_item(index))
            .expect("append");
    }

    let items = store.replay_items(&thread.id).expect("replay");
    let ids: Vec<String> = items.into_iter().map(|item| item.id).collect();

    assert_eq!(ids, vec!["item_0", "item_1", "item_2", "item_3", "item_4"]);
}

#[test]
fn thread_store_can_replay_one_hundred_items_in_stable_order() {
    let store = SqliteThreadStore::in_memory().expect("store");
    let thread = store
        .create_thread(ThreadStartParams {
            cwd: None,
            title: None,
            model: None,
            ephemeral: false,
        })
        .expect("create");

    for index in 0..100 {
        store
            .append_item(&thread.id, "turn_1", test_item(index))
            .expect("append");
    }

    let items = store.replay_items(&thread.id).expect("replay");

    assert_eq!(items.len(), 100);
    assert_eq!(items.first().expect("first").id, "item_0");
    assert_eq!(items.last().expect("last").id, "item_99");
}

fn test_item(index: usize) -> AppItem {
    AppItem {
        id: format!("item_{index}"),
        run_event_id: format!("evt_{index}"),
        kind: AppItemKind::AgentMessage,
        status: AppItemStatus::Completed,
        created_at_ms: index as u128,
        completed_at_ms: Some(index as u128),
        content: Some(serde_json::json!({ "text": format!("message {index}") })),
    }
}
