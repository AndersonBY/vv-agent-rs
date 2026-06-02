use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::sync::mpsc;
use tokio::sync::Mutex;
use vv_agent::app_server::outgoing::OutgoingEnvelope;
use vv_agent::app_server::processor::MessageProcessor;
use vv_agent::app_server::protocol::{
    AppItem, AppItemKind, AppItemStatus, ApprovalDecision, JsonRpcMessage, JsonRpcNotification,
    JsonRpcRequest, JsonRpcResponse, RequestId, ServerNotification, ThreadArchiveResponse,
    ThreadListResponse, ThreadResumeResponse, ThreadStartParams, ThreadStartResponse,
    TurnStartResponse,
};
use vv_agent::app_server::request_serialization::{
    RequestSerializationAccess, RequestSerializationQueue, RequestSerializationQueueKey,
    RequestSerializationScope,
};
use vv_agent::app_server::thread_store::SqliteThreadStore;
use vv_agent::app_server::transport::ConnectionId;
use vv_agent::{
    Agent, FunctionTool, LLMResponse, ModelRef, Runner, ScriptedModelProvider, ToolCall, ToolOutput,
};

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

#[test]
fn thread_store_archive_keeps_replay_items_readable() {
    let store = SqliteThreadStore::in_memory().expect("store");
    let thread = store
        .create_thread(ThreadStartParams {
            cwd: None,
            title: None,
            model: None,
            ephemeral: false,
        })
        .expect("create");
    store
        .append_item(&thread.id, "turn_1", test_item(1))
        .expect("append");

    store.archive_thread(&thread.id).expect("archive");

    let items = store.replay_items(&thread.id).expect("replay");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, "item_1");
}

#[tokio::test]
async fn thread_resume_replays_items_and_subscribes_second_client_to_active_turn() {
    let (mut processor, mut outgoing) = approval_processor();
    let client_a = ConnectionId::new(1);
    let client_b = ConnectionId::new(2);

    initialize(&mut processor, &mut outgoing, client_a, 1).await;
    let thread_id = start_thread(&mut processor, &mut outgoing, client_a, 2, "active").await;
    processor
        .process_message(
            client_a,
            request(
                3,
                "turn/start",
                json!({
                    "threadId": thread_id,
                    "input": [{"text": "run approval tool"}],
                    "model": "approval-model"
                }),
            ),
        )
        .await;
    let turn: TurnStartResponse =
        decode_response(expect_response_for_connection(&mut outgoing, client_a).await);
    let approval = next_notification_for_connection_matching(&mut outgoing, client_a, |message| {
        matches!(message, ServerNotification::ApprovalRequested(_))
    })
    .await;
    assert!(matches!(approval, ServerNotification::ApprovalRequested(_)));
    let server_request = next_server_request(&mut outgoing).await;

    initialize(&mut processor, &mut outgoing, client_b, 10).await;
    processor
        .process_message(
            client_b,
            request(11, "thread/resume", json!({ "threadId": thread_id })),
        )
        .await;
    let resumed: ThreadResumeResponse =
        decode_response(expect_response_for_connection(&mut outgoing, client_b).await);
    assert_eq!(resumed.thread.id, thread_id);
    assert_eq!(
        resumed.active_turn.as_ref().map(|turn| turn.id.as_str()),
        Some(turn.turn.id.as_str())
    );
    assert!(resumed
        .items
        .iter()
        .any(|item| item.kind == AppItemKind::ApprovalRequest));

    processor
        .process_message(
            server_request.connection_id,
            JsonRpcMessage::Response(JsonRpcResponse {
                id: server_request.request.id,
                result: json!({ "decision": "allow" }),
            }),
        )
        .await;

    let resolved = next_notification_for_connection_matching(&mut outgoing, client_b, |message| {
        matches!(message, ServerNotification::ApprovalResolved(_))
    })
    .await;
    let ServerNotification::ApprovalResolved(resolved) = resolved else {
        unreachable!("matched approval resolved");
    };
    assert_eq!(resolved.decision, ApprovalDecision::Allow);

    let completed = next_notification_for_connection_matching(&mut outgoing, client_b, |message| {
        matches!(message, ServerNotification::TurnCompleted(_))
    })
    .await;
    assert!(matches!(completed, ServerNotification::TurnCompleted(_)));
}

#[tokio::test]
async fn thread_list_filters_archived_threads_and_archive_emits_notification() {
    let (mut processor, mut outgoing) = approval_processor();
    let connection_id = ConnectionId::new(1);
    initialize(&mut processor, &mut outgoing, connection_id, 1).await;
    let keep_id = start_thread(&mut processor, &mut outgoing, connection_id, 2, "keep").await;
    let archive_id = start_thread(&mut processor, &mut outgoing, connection_id, 3, "archive").await;

    processor
        .process_message(
            connection_id,
            request(4, "thread/archive", json!({ "threadId": archive_id })),
        )
        .await;
    let _: ThreadArchiveResponse =
        decode_response(expect_response_for_connection(&mut outgoing, connection_id).await);
    let archived =
        next_notification_for_connection_matching(&mut outgoing, connection_id, |message| {
            matches!(message, ServerNotification::ThreadArchived(_))
        })
        .await;
    assert!(matches!(archived, ServerNotification::ThreadArchived(_)));

    processor
        .process_message(connection_id, request(5, "thread/list", json!({})))
        .await;
    let active: ThreadListResponse =
        decode_response(expect_response_for_connection(&mut outgoing, connection_id).await);
    assert_eq!(active.threads.len(), 1);
    assert_eq!(active.threads[0].id, keep_id);

    processor
        .process_message(
            connection_id,
            request(6, "thread/list", json!({ "archived": true })),
        )
        .await;
    let archived: ThreadListResponse =
        decode_response(expect_response_for_connection(&mut outgoing, connection_id).await);
    assert_eq!(archived.threads.len(), 1);
    assert_eq!(archived.threads[0].id, archive_id);

    processor
        .process_message(
            connection_id,
            request(
                7,
                "thread/list",
                json!({ "includeArchived": true, "offset": 1, "limit": 1 }),
            ),
        )
        .await;
    let paged: ThreadListResponse =
        decode_response(expect_response_for_connection(&mut outgoing, connection_id).await);
    assert_eq!(paged.threads.len(), 1);
}

#[tokio::test]
async fn request_serialization_same_thread_exclusive_requests_run_fifo() {
    let queue = RequestSerializationQueue::default();
    let observed = Arc::new(Mutex::new(Vec::new()));

    let first_observed = observed.clone();
    let first_queue = queue.clone();
    let first = tokio::spawn(async move {
        first_queue
            .run(
                RequestSerializationScope::exclusive_thread("thread_1"),
                async move {
                    first_observed.lock().await.push("first:start");
                    tokio::time::sleep(Duration::from_millis(25)).await;
                    first_observed.lock().await.push("first:end");
                },
            )
            .await;
    });
    tokio::time::sleep(Duration::from_millis(5)).await;

    let second_observed = observed.clone();
    let second = tokio::spawn(async move {
        queue
            .run(
                RequestSerializationScope::exclusive_thread("thread_1"),
                async move {
                    second_observed.lock().await.push("second:start");
                    second_observed.lock().await.push("second:end");
                },
            )
            .await;
    });

    first.await.expect("first task");
    second.await.expect("second task");

    assert_eq!(
        observed.lock().await.as_slice(),
        ["first:start", "first:end", "second:start", "second:end"]
    );
}

#[tokio::test]
async fn request_serialization_shared_reads_with_same_thread_run_together() {
    let queue = RequestSerializationQueue::default();
    let active = Arc::new(Mutex::new(0usize));
    let max_active = Arc::new(Mutex::new(0usize));

    let first = spawn_shared_read(queue.clone(), active.clone(), max_active.clone());
    let second = spawn_shared_read(queue, active.clone(), max_active.clone());

    first.await.expect("first shared read");
    second.await.expect("second shared read");

    assert_eq!(*max_active.lock().await, 2);
}

#[tokio::test]
async fn request_serialization_exclusive_waits_behind_earlier_shared_reads() {
    let queue = RequestSerializationQueue::default();
    let observed = Arc::new(Mutex::new(Vec::new()));

    let shared_observed = observed.clone();
    let shared_queue = queue.clone();
    let shared = tokio::spawn(async move {
        shared_queue
            .run(
                RequestSerializationScope::shared_thread("thread_1"),
                async move {
                    shared_observed.lock().await.push("shared:start");
                    tokio::time::sleep(Duration::from_millis(25)).await;
                    shared_observed.lock().await.push("shared:end");
                },
            )
            .await;
    });
    tokio::time::sleep(Duration::from_millis(5)).await;

    let exclusive_observed = observed.clone();
    let exclusive = tokio::spawn(async move {
        queue
            .run(
                RequestSerializationScope::exclusive_thread("thread_1"),
                async move {
                    exclusive_observed.lock().await.push("exclusive:start");
                    exclusive_observed.lock().await.push("exclusive:end");
                },
            )
            .await;
    });

    shared.await.expect("shared read");
    exclusive.await.expect("exclusive write");

    assert_eq!(
        observed.lock().await.as_slice(),
        [
            "shared:start",
            "shared:end",
            "exclusive:start",
            "exclusive:end"
        ]
    );
}

#[test]
fn request_serialization_scope_derives_thread_key_from_thread_id() {
    let turn_start = RequestSerializationScope::for_method(
        "turn/start",
        Some(&json!({ "threadId": "thread_1" })),
    )
    .expect("turn scope");
    assert_eq!(
        turn_start.key(),
        &RequestSerializationQueueKey::thread("thread_1")
    );
    assert_eq!(turn_start.access(), RequestSerializationAccess::Exclusive);

    let thread_read = RequestSerializationScope::for_method(
        "thread/read",
        Some(&json!({ "threadId": "thread_1" })),
    )
    .expect("read scope");
    assert_eq!(
        thread_read.key(),
        &RequestSerializationQueueKey::thread("thread_1")
    );
    assert_eq!(thread_read.access(), RequestSerializationAccess::Shared);
}

#[test]
fn request_serialization_scope_uses_global_shared_keys_for_model_and_schema_reads() {
    let model =
        RequestSerializationScope::for_method("model/list", Some(&json!({}))).expect("model scope");
    assert_eq!(model.key(), &RequestSerializationQueueKey::global("model"));
    assert_eq!(model.access(), RequestSerializationAccess::Shared);

    let schema =
        RequestSerializationScope::for_method("schema/export", None).expect("schema scope");
    assert_eq!(
        schema.key(),
        &RequestSerializationQueueKey::global("schema")
    );
    assert_eq!(schema.access(), RequestSerializationAccess::Shared);
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

fn spawn_shared_read(
    queue: RequestSerializationQueue,
    active: Arc<Mutex<usize>>,
    max_active: Arc<Mutex<usize>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        queue
            .run(
                RequestSerializationScope::shared_thread("thread_1"),
                async move {
                    let current = {
                        let mut active = active.lock().await;
                        *active += 1;
                        *active
                    };
                    {
                        let mut max_active = max_active.lock().await;
                        *max_active = (*max_active).max(current);
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    *active.lock().await -= 1;
                },
            )
            .await;
    })
}

struct ServerRequestEnvelope {
    connection_id: ConnectionId,
    request: JsonRpcRequest,
}

fn approval_processor() -> (MessageProcessor, mpsc::Receiver<OutgoingEnvelope>) {
    let dangerous = FunctionTool::builder("dangerous")
        .description("Requires approval.")
        .json_schema(json!({"type":"object","properties":{},"required":[]}))
        .handler(|_ctx, _args: serde_json::Value| async move { Ok(ToolOutput::text("allowed")) })
        .build()
        .expect("tool");
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "approval-model",
            vec![
                LLMResponse::with_tool_calls(
                    "",
                    vec![ToolCall::from_raw_arguments(
                        "call_1",
                        "dangerous",
                        json!({}),
                    )],
                ),
                LLMResponse::with_tool_calls(
                    "",
                    vec![ToolCall::from_raw_arguments(
                        "finish",
                        "task_finish",
                        json!({"message":"done"}),
                    )],
                ),
            ],
        ))
        .workspace(".")
        .build()
        .expect("runner");
    let agent = Agent::builder("approver")
        .instructions("Call dangerous, then finish.")
        .model(ModelRef::named("approval-model"))
        .tool(dangerous)
        .build()
        .expect("agent");
    MessageProcessor::new_for_tests_with_runtime(
        64,
        runner,
        agent,
        SqliteThreadStore::in_memory().expect("store"),
    )
}

async fn initialize(
    processor: &mut MessageProcessor,
    outgoing: &mut mpsc::Receiver<OutgoingEnvelope>,
    connection_id: ConnectionId,
    request_id: i64,
) {
    processor
        .process_message(
            connection_id,
            request(
                request_id,
                "initialize",
                json!({
                    "clientInfo": {
                        "name": "test_client",
                        "title": "Test Client",
                        "version": "1.0.0"
                    },
                    "capabilities": {
                        "experimentalApi": false,
                        "optOutNotificationMethods": []
                    }
                }),
            ),
        )
        .await;
    let _ = expect_response_for_connection(outgoing, connection_id).await;
    processor
        .process_message(
            connection_id,
            JsonRpcMessage::Notification(JsonRpcNotification {
                method: "initialized".to_string(),
                params: None,
            }),
        )
        .await;
}

async fn start_thread(
    processor: &mut MessageProcessor,
    outgoing: &mut mpsc::Receiver<OutgoingEnvelope>,
    connection_id: ConnectionId,
    request_id: i64,
    title: &str,
) -> String {
    processor
        .process_message(
            connection_id,
            request(
                request_id,
                "thread/start",
                json!({
                    "title": title,
                    "model": "approval-model",
                    "ephemeral": false
                }),
            ),
        )
        .await;
    let response: ThreadStartResponse =
        decode_response(expect_response_for_connection(outgoing, connection_id).await);
    let _ = next_notification_for_connection_matching(outgoing, connection_id, |message| {
        matches!(message, ServerNotification::ThreadStarted(_))
    })
    .await;
    response.thread.id
}

fn request(id: i64, method: &str, params: serde_json::Value) -> JsonRpcMessage {
    JsonRpcMessage::Request(JsonRpcRequest {
        id: RequestId::Integer(id),
        method: method.to_string(),
        params: Some(params),
    })
}

async fn expect_response_for_connection(
    rx: &mut mpsc::Receiver<OutgoingEnvelope>,
    connection_id: ConnectionId,
) -> JsonRpcResponse {
    loop {
        let envelope = next_outgoing_envelope(rx).await;
        if envelope.connection_id != connection_id {
            continue;
        }
        match envelope.message {
            JsonRpcMessage::Response(response) => return response,
            JsonRpcMessage::Error(error) => panic!("expected response, got error: {error:?}"),
            _ => {}
        }
    }
}

async fn next_notification_for_connection_matching(
    rx: &mut mpsc::Receiver<OutgoingEnvelope>,
    connection_id: ConnectionId,
    predicate: impl Fn(&ServerNotification) -> bool,
) -> ServerNotification {
    loop {
        let envelope = next_outgoing_envelope(rx).await;
        if envelope.connection_id != connection_id {
            continue;
        }
        match envelope.message {
            JsonRpcMessage::Notification(notification) => {
                let notification = decode_notification(notification);
                if predicate(&notification) {
                    return notification;
                }
            }
            JsonRpcMessage::Error(error) => {
                panic!("expected notification, got error: {error:?}");
            }
            _ => {}
        }
    }
}

async fn next_server_request(rx: &mut mpsc::Receiver<OutgoingEnvelope>) -> ServerRequestEnvelope {
    loop {
        let envelope = next_outgoing_envelope(rx).await;
        match envelope.message {
            JsonRpcMessage::Request(request) => {
                return ServerRequestEnvelope {
                    connection_id: envelope.connection_id,
                    request,
                };
            }
            JsonRpcMessage::Error(error) => {
                panic!("expected server request, got error: {error:?}");
            }
            _ => {}
        }
    }
}

async fn next_outgoing_envelope(rx: &mut mpsc::Receiver<OutgoingEnvelope>) -> OutgoingEnvelope {
    tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .expect("message timeout")
        .expect("outgoing message")
}

fn decode_response<T: serde::de::DeserializeOwned>(response: JsonRpcResponse) -> T {
    serde_json::from_value(response.result).expect("response payload")
}

fn decode_notification(notification: JsonRpcNotification) -> ServerNotification {
    let value = match notification.params {
        Some(params) => json!({
            "method": notification.method,
            "params": params,
        }),
        None => json!({
            "method": notification.method,
        }),
    };
    serde_json::from_value(value).expect("server notification")
}
