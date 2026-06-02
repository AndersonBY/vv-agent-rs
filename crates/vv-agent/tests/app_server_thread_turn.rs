use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::json;
use tokio::sync::mpsc;
use vv_agent::app_server::protocol::{
    map_run_event_to_notifications, AppItemKind, AppItemStatus, JsonRpcMessage,
    JsonRpcNotification, JsonRpcRequest, JsonRpcResponse, RequestId, ServerNotification,
    ThreadReadResponse, ThreadStartResponse, TurnStartResponse, TurnStatus, UserInput,
};
use vv_agent::app_server::{outgoing::OutgoingEnvelope, processor::MessageProcessor};
use vv_agent::app_server::{thread_store::SqliteThreadStore, transport::ConnectionId};
use vv_agent::{
    Agent, AgentStatus, LLMResponse, ModelRef, RunEvent, Runner, ScriptedModelProvider, ToolCall,
    ToolStatus,
};

#[test]
fn item_mapping_assistant_delta_becomes_agent_message_delta() {
    let event = RunEvent::assistant_delta("run_1", "trace_1", "assistant", 1, "hello");

    let notifications = map_run_event_to_notifications("thread_1", "turn_1", &event);

    let [ServerNotification::AgentMessageDelta(delta)] = notifications.as_slice() else {
        panic!("expected agent message delta");
    };
    assert_eq!(delta.thread_id, "thread_1");
    assert_eq!(delta.turn_id, "turn_1");
    assert_eq!(delta.item_id, event.event_id().as_str());
    assert_eq!(delta.delta, "hello");
}

#[test]
fn item_mapping_tool_call_started_becomes_started_tool_item() {
    let event = RunEvent::tool_call_started(
        "run_1",
        "trace_1",
        "assistant",
        1,
        "call_1",
        "bash",
        json!({"cmd": "cargo test"}),
    );

    let notifications = map_run_event_to_notifications("thread_1", "turn_1", &event);

    let [ServerNotification::ItemStarted(started)] = notifications.as_slice() else {
        panic!("expected item started");
    };
    assert_eq!(started.item.run_event_id, event.event_id().as_str());
    assert_eq!(started.item.kind, AppItemKind::ToolCall);
    assert_eq!(started.item.status, AppItemStatus::InProgress);
    assert_eq!(
        started.item.content.as_ref().expect("content")["toolName"],
        "bash"
    );
}

#[test]
fn item_mapping_tool_call_completed_becomes_completed_item() {
    let event = RunEvent::tool_call_completed(
        "run_1",
        "trace_1",
        "assistant",
        Some(1),
        "call_1",
        "bash",
        ToolStatus::Success,
    );

    let notifications = map_run_event_to_notifications("thread_1", "turn_1", &event);

    let [ServerNotification::ItemCompleted(completed)] = notifications.as_slice() else {
        panic!("expected item completed");
    };
    assert_eq!(completed.item.kind, AppItemKind::ToolCall);
    assert_eq!(completed.item.status, AppItemStatus::Completed);
    assert_eq!(completed.item.completed_at_ms, Some(event.created_at_ms()));
}

#[test]
fn item_mapping_approval_requested_becomes_approval_notification() {
    let event = RunEvent::approval_requested(
        "run_1",
        "trace_1",
        "assistant",
        "approval_1",
        "call_1",
        "bash",
        "Run cargo test",
    );

    let notifications = map_run_event_to_notifications("thread_1", "turn_1", &event);

    let [ServerNotification::ApprovalRequested(approval)] = notifications.as_slice() else {
        panic!("expected approval requested");
    };
    assert_eq!(approval.thread_id, "thread_1");
    assert_eq!(approval.turn_id, "turn_1");
    assert_eq!(approval.request_id, "approval_1");
    assert_eq!(approval.tool_name, "bash");
}

#[test]
fn item_mapping_run_completed_becomes_turn_completed() {
    let event = RunEvent::run_completed("run_1", "trace_1", "assistant", AgentStatus::Completed);

    let notifications = map_run_event_to_notifications("thread_1", "turn_1", &event);

    let [ServerNotification::TurnCompleted(completed)] = notifications.as_slice() else {
        panic!("expected turn completed");
    };
    assert_eq!(completed.turn.id, "turn_1");
    assert_eq!(completed.turn.thread_id, "thread_1");
    assert_eq!(completed.turn.run_id, "run_1");
    assert_eq!(completed.turn.status, TurnStatus::Completed);
    assert_eq!(completed.turn.completed_at_ms, Some(event.created_at_ms()));
}

#[tokio::test]
async fn json_rpc_thread_turn_streams_notifications_and_replays_items() {
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "demo-model",
            vec![finish_response("hello world")],
        ))
        .workspace(".")
        .build()
        .expect("runner");
    let agent = Agent::builder("assistant")
        .instructions("Answer the user, then finish.")
        .model(ModelRef::named("demo-model"))
        .build()
        .expect("agent");
    let store = SqliteThreadStore::in_memory().expect("store");
    let (mut processor, mut outgoing) =
        MessageProcessor::new_for_tests_with_runtime(32, runner, agent, store);
    let connection_id = ConnectionId::new(1);

    processor
        .process_message(connection_id, initialize_request(1))
        .await;
    let _initialize = expect_response(&mut outgoing).await;
    processor
        .process_message(connection_id, initialized_notification())
        .await;

    processor
        .process_message(
            connection_id,
            request(
                2,
                "thread/start",
                json!({
                    "title": "demo",
                    "model": "demo-model",
                    "ephemeral": false
                }),
            ),
        )
        .await;
    let thread_response: ThreadStartResponse =
        decode_response(expect_response(&mut outgoing).await);
    let thread_id = thread_response.thread.id.clone();
    assert_eq!(thread_response.thread.title.as_deref(), Some("demo"));
    assert!(matches!(
        expect_notification(&mut outgoing).await,
        ServerNotification::ThreadStarted(_)
    ));

    processor
        .process_message(
            connection_id,
            request(
                3,
                "turn/start",
                json!({
                    "threadId": thread_id,
                    "input": [{"text": "say hello"}],
                    "model": "demo-model"
                }),
            ),
        )
        .await;
    let turn_response: TurnStartResponse = decode_response(expect_response(&mut outgoing).await);
    let turn_id = turn_response.turn.id.clone();
    assert_eq!(turn_response.turn.thread_id, thread_id);
    assert_eq!(
        turn_response.turn.input,
        vec![UserInput {
            text: "say hello".into()
        }]
    );

    let started = expect_notification(&mut outgoing).await;
    assert!(matches!(started, ServerNotification::TurnStarted(_)));

    let delta = next_notification_matching(&mut outgoing, |notification| {
        matches!(notification, ServerNotification::AgentMessageDelta(_))
    })
    .await;
    let ServerNotification::AgentMessageDelta(delta) = delta else {
        unreachable!("matched delta")
    };
    assert_eq!(delta.thread_id, thread_id);
    assert_eq!(delta.turn_id, turn_id);
    assert_eq!(delta.delta, "hello world");

    let item_completed = next_notification_matching(&mut outgoing, |notification| {
        matches!(notification, ServerNotification::ItemCompleted(_))
    })
    .await;
    assert!(matches!(
        item_completed,
        ServerNotification::ItemCompleted(_)
    ));

    let completed = next_notification_matching(&mut outgoing, |notification| {
        matches!(notification, ServerNotification::TurnCompleted(_))
    })
    .await;
    let ServerNotification::TurnCompleted(completed) = completed else {
        unreachable!("matched turn completed")
    };
    assert_eq!(completed.turn.id, turn_id);
    assert_eq!(completed.turn.status, TurnStatus::Completed);

    processor
        .process_message(
            connection_id,
            request(4, "thread/read", json!({ "threadId": thread_id })),
        )
        .await;
    let read: ThreadReadResponse = decode_response(expect_response(&mut outgoing).await);
    assert_eq!(read.thread.id, thread_id);
    assert!(read
        .items
        .iter()
        .any(|item| item.kind == AppItemKind::AgentMessage));
    assert!(read
        .items
        .iter()
        .any(|item| item.kind == AppItemKind::ToolCall));
}

fn request(id: i64, method: &str, params: serde_json::Value) -> JsonRpcMessage {
    JsonRpcMessage::Request(JsonRpcRequest {
        id: RequestId::Integer(id),
        method: method.to_string(),
        params: Some(params),
    })
}

fn initialize_request(id: i64) -> JsonRpcMessage {
    request(
        id,
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
    )
}

fn initialized_notification() -> JsonRpcMessage {
    JsonRpcMessage::Notification(JsonRpcNotification {
        method: "initialized".to_string(),
        params: None,
    })
}

async fn expect_response(rx: &mut mpsc::Receiver<OutgoingEnvelope>) -> JsonRpcResponse {
    let envelope = tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .expect("message timeout")
        .expect("outgoing message");
    let JsonRpcMessage::Response(response) = envelope.message else {
        panic!("expected response, got {:?}", envelope.message);
    };
    response
}

async fn expect_notification(rx: &mut mpsc::Receiver<OutgoingEnvelope>) -> ServerNotification {
    let envelope = tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .expect("message timeout")
        .expect("outgoing message");
    let JsonRpcMessage::Notification(notification) = envelope.message else {
        panic!("expected notification, got {:?}", envelope.message);
    };
    decode_notification(notification)
}

async fn next_notification_matching(
    rx: &mut mpsc::Receiver<OutgoingEnvelope>,
    predicate: impl Fn(&ServerNotification) -> bool,
) -> ServerNotification {
    loop {
        let notification = expect_notification(rx).await;
        if predicate(&notification) {
            return notification;
        }
    }
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

fn finish_response(message: &str) -> LLMResponse {
    let mut args = BTreeMap::new();
    args.insert("message".to_string(), json!(message));
    LLMResponse::with_tool_calls(message, vec![ToolCall::new("finish", "task_finish", args)])
}
