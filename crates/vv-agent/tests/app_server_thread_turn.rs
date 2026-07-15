use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::mpsc;
use vv_agent::app_server::protocol::{
    map_run_event_to_notifications, AppItemKind, AppItemStatus, JsonRpcMessage,
    JsonRpcNotification, JsonRpcRequest, JsonRpcResponse, RequestId, ServerNotification,
    ThreadReadResponse, ThreadStartResponse, TurnStartResponse, TurnStatus,
};
use vv_agent::app_server::{outgoing::OutgoingEnvelope, processor::MessageProcessor};
use vv_agent::app_server::{thread_store::SqliteThreadStore, transport::ConnectionId};
use vv_agent::events::ApprovalAction;
use vv_agent::{
    Agent, AgentStatus, CompletionReason, FunctionTool, LLMResponse, ModelRef, NoToolPolicy,
    RunEvent, RunEventPayload, Runner, ScriptedModelProvider, ToolCall, ToolOutput, ToolStatus,
};

const APP_SERVER_CONTRACT: &str = include_str!("fixtures/parity/app_server_observable_v1.json");

fn status_projection(name: &str) -> Value {
    let contract: Value = serde_json::from_str(APP_SERVER_CONTRACT).expect("App Server contract");
    contract["terminal"]["agentStatusProjection"]
        .as_array()
        .expect("agent status projections")
        .iter()
        .find(|case| case["name"] == name)
        .unwrap_or_else(|| panic!("missing App Server projection {name}"))
        .clone()
}

#[test]
fn item_mapping_assistant_delta_becomes_agent_message_delta() {
    let event = RunEvent::assistant_delta("run_1", "trace_1", "assistant", 1, "hello");

    let notifications = map_run_event_to_notifications("thread_1", "turn_1", &event);

    let [ServerNotification::AgentMessageDelta(delta)] = notifications.as_slice() else {
        panic!("expected agent message delta");
    };
    assert_eq!(delta.item.thread_id, "thread_1");
    assert_eq!(delta.item.turn_id, "turn_1");
    assert_eq!(
        delta.item.item_id,
        format!("item_{}", event.event_id().as_str())
    );
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

    let Some(ServerNotification::ItemStarted(started)) = notifications.first() else {
        panic!("expected item started first");
    };
    assert_eq!(
        started.item.item_id,
        format!("item_{}", event.event_id().as_str())
    );
    assert_eq!(started.item.kind, AppItemKind::ToolCall);
    assert_eq!(started.item.status, AppItemStatus::Started);
    assert_eq!(started.item.payload["toolName"], "bash");
    assert!(matches!(
        notifications.get(1),
        Some(ServerNotification::ToolCallDelta(_))
    ));
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
    assert_eq!(
        completed.item.updated_at,
        event.created_at_ms() as f64 / 1000.0
    );
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
    )
    .with_metadata("arguments", json!({"cmd": "cargo test"}))
    .with_metadata("tool_name", json!("bash"));

    let notifications = map_run_event_to_notifications("thread_1", "turn_1", &event);

    let [ServerNotification::ItemStarted(started), ServerNotification::ApprovalRequested(approval)] =
        notifications.as_slice()
    else {
        panic!("expected approval item and request");
    };
    assert_eq!(started.item.payload["message"], "Run cargo test");
    assert_eq!(
        started.item.payload["arguments"],
        json!({"cmd": "cargo test"})
    );
    assert_eq!(approval.thread_id, "thread_1");
    assert_eq!(approval.turn_id, "turn_1");
    assert_eq!(approval.request_id, "approval_1");
    assert_eq!(approval.tool_name, "bash");
    assert_eq!(approval.arguments, json!({"cmd": "cargo test"}));
}

#[test]
fn item_mapping_approval_resolved_preserves_reason_and_metadata() {
    let event = RunEvent::new(
        "run_1",
        "trace_1",
        "assistant",
        Some(1),
        RunEventPayload::ApprovalResolved {
            request_id: "approval_1".to_string(),
            tool_name: "bash".to_string(),
            tool_call_id: "call_1".to_string(),
            approved: true,
        },
    )
    .with_approval_action(ApprovalAction::AllowSession)
    .with_metadata("action", json!("allow_session"))
    .with_metadata("reason", json!("approved by owner"))
    .with_metadata("decision_metadata", json!({"ticket": 7}));

    let notifications = map_run_event_to_notifications("thread_1", "turn_1", &event);

    let [ServerNotification::ItemCompleted(completed), ServerNotification::ApprovalResolved(resolved)] =
        notifications.as_slice()
    else {
        panic!("expected approval completion and resolution");
    };
    assert_eq!(completed.item.payload["reason"], "approved by owner");
    assert_eq!(
        completed.item.payload["decisionMetadata"],
        json!({"ticket": 7})
    );
    assert_eq!(resolved.reason, "approved by owner");
    assert_eq!(resolved.metadata["ticket"], 7);
}

#[test]
fn item_mapping_leaves_run_completion_to_runtime_adapter() {
    let event = RunEvent::run_completed("run_1", "trace_1", "assistant", AgentStatus::Completed);

    let notifications = map_run_event_to_notifications("thread_1", "turn_1", &event);

    assert!(notifications.is_empty());
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
                    "agentKey": "default",
                    "metadata": {"title": "demo"}
                }),
            ),
        )
        .await;
    let thread_response: ThreadStartResponse =
        decode_response(expect_response(&mut outgoing).await);
    let thread_id = thread_response.thread_id.clone();
    assert_eq!(thread_response.agent_key, "default");
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
                    "input": [{"type": "text", "text": "say hello"}]
                }),
            ),
        )
        .await;
    let turn_response: TurnStartResponse = decode_response(expect_response(&mut outgoing).await);
    let turn_id = turn_response.turn_id.clone();
    assert_eq!(turn_response.thread_id, thread_id);
    assert_eq!(turn_response.status, TurnStatus::Running);

    let started = next_notification_matching(&mut outgoing, |notification| {
        matches!(notification, ServerNotification::TurnStarted(_))
    })
    .await;
    assert!(matches!(started, ServerNotification::TurnStarted(_)));

    let item_completed = next_notification_matching(&mut outgoing, |notification| {
        matches!(
            notification,
            ServerNotification::ItemCompleted(completed)
                if completed.item.kind == AppItemKind::AgentMessage
        )
    })
    .await;
    let ServerNotification::ItemCompleted(item_completed) = item_completed else {
        unreachable!("matched completed agent message")
    };
    assert_eq!(item_completed.item.thread_id, thread_id);
    assert_eq!(item_completed.item.turn_id, turn_id);
    assert_eq!(item_completed.item.payload["text"], "hello world");

    let completed = next_notification_matching(&mut outgoing, |notification| {
        matches!(notification, ServerNotification::TurnCompleted(_))
    })
    .await;
    let ServerNotification::TurnCompleted(completed) = completed else {
        unreachable!("matched turn completed")
    };
    assert_eq!(completed.turn_id, turn_id);
    assert_eq!(completed.status, TurnStatus::Completed);
    assert_eq!(completed.final_output.as_deref(), Some("hello world"));
    assert_eq!(completed.completion_reason.as_deref(), Some("tool_finish"));
    assert_eq!(
        completed.completion_tool_name.as_deref(),
        Some("task_finish")
    );
    assert_eq!(completed.partial_output, None);

    processor
        .process_message(
            connection_id,
            request(4, "thread/read", json!({ "threadId": thread_id })),
        )
        .await;
    let read: ThreadReadResponse = decode_response(expect_response(&mut outgoing).await);
    assert_eq!(read.thread.thread_id, thread_id);
    assert!(read
        .items
        .iter()
        .any(|item| item.kind == AppItemKind::AgentMessage));
    assert!(read
        .items
        .iter()
        .any(|item| item.kind == AppItemKind::ToolCall));
    let persisted = read
        .turns
        .iter()
        .find(|turn| turn.turn_id == turn_id)
        .expect("persisted completed turn");
    assert_eq!(persisted.result["completionReason"], "tool_finish");
    assert_eq!(persisted.result["completionToolName"], "task_finish");
    assert!(!persisted.result.contains_key("partialOutput"));
}

#[tokio::test]
async fn wait_user_turn_is_interrupted_in_notification_and_persisted_snapshot() {
    let expected = status_projection("wait_user_is_interrupted_without_error");
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "demo-model",
            vec![LLMResponse::new("need more details")],
        ))
        .workspace(".")
        .build()
        .expect("runner");
    let agent = Agent::builder("assistant")
        .instructions("Wait for clarification.")
        .model(ModelRef::named("demo-model"))
        .no_tool_policy(NoToolPolicy::WaitUser)
        .build()
        .expect("agent");
    let store = SqliteThreadStore::in_memory().expect("store");
    let (mut processor, mut outgoing) =
        MessageProcessor::new_for_tests_with_runtime(32, runner, agent, store);
    let connection_id = ConnectionId::new(2);

    processor
        .process_message(connection_id, initialize_request(1))
        .await;
    let _ = expect_response(&mut outgoing).await;
    processor
        .process_message(connection_id, initialized_notification())
        .await;
    processor
        .process_message(
            connection_id,
            request(2, "thread/start", json!({"agentKey": "default"})),
        )
        .await;
    let thread: ThreadStartResponse = decode_response(expect_response(&mut outgoing).await);
    let _ = next_notification_matching(&mut outgoing, |notification| {
        matches!(notification, ServerNotification::ThreadStarted(_))
    })
    .await;
    processor
        .process_message(
            connection_id,
            request(
                3,
                "turn/start",
                json!({
                    "threadId": thread.thread_id,
                    "input": [{"type": "text", "text": "ambiguous request"}]
                }),
            ),
        )
        .await;
    let started: TurnStartResponse = decode_response(expect_response(&mut outgoing).await);
    let completed = next_notification_matching(&mut outgoing, |notification| {
        matches!(notification, ServerNotification::TurnCompleted(_))
    })
    .await;
    let ServerNotification::TurnCompleted(completed) = completed else {
        unreachable!("matched turn completion")
    };

    assert_eq!(
        serde_json::to_value(completed.status).expect("turn status"),
        expected["turnStatus"]
    );
    assert_eq!(
        completed.completion_reason.as_deref(),
        expected["completionReason"].as_str()
    );
    assert_eq!(completed.completion_tool_name, None);
    assert_eq!(
        completed.partial_output.as_deref(),
        Some("need more details")
    );
    assert_eq!(completed.final_output.as_deref(), Some("need more details"));
    assert_eq!(
        completed.error.is_some(),
        expected["errorField"] == "present"
    );
    assert_eq!(
        CompletionReason::parse(completed.completion_reason.as_deref().unwrap()),
        Some(CompletionReason::WaitUser)
    );

    processor
        .process_message(
            connection_id,
            request(4, "thread/read", json!({"threadId": thread.thread_id})),
        )
        .await;
    let read: ThreadReadResponse = decode_response(expect_response(&mut outgoing).await);
    let persisted = read
        .turns
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .expect("persisted wait turn");
    assert_eq!(
        serde_json::to_value(persisted.status).expect("persisted turn status"),
        expected["turnStatus"]
    );
    assert_eq!(
        persisted.result["completionReason"],
        expected["completionReason"]
    );
    assert_eq!(persisted.result["partialOutput"], "need more details");
    assert_eq!(
        persisted.result.contains_key("error"),
        expected["errorField"] == "present"
    );
}

#[tokio::test]
async fn cancelled_failed_turn_stays_failed_in_notification_and_persisted_snapshot() {
    let expected = status_projection("cancelled_failure_stays_failed");
    let approval_tool = FunctionTool::builder("approval_tool")
        .description("Pause at the App Server approval boundary.")
        .json_schema(json!({"type": "object", "properties": {}, "required": []}))
        .needs_approval(true)
        .handler(|_context, _arguments: serde_json::Value| async move {
            Ok(ToolOutput::text("should not execute"))
        })
        .build()
        .expect("approval tool");
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "cancel-model",
            vec![LLMResponse::with_tool_calls(
                "waiting for approval",
                vec![ToolCall::from_raw_arguments(
                    "approval-call",
                    "approval_tool",
                    json!({}),
                )],
            )],
        ))
        .workspace(".")
        .build()
        .expect("runner");
    let agent = Agent::builder("assistant")
        .instructions("Finish after the blocking model responds.")
        .model(ModelRef::named("cancel-model"))
        .tool(approval_tool)
        .build()
        .expect("agent");
    let store = SqliteThreadStore::in_memory().expect("store");
    let (mut processor, mut outgoing) =
        MessageProcessor::new_for_tests_with_runtime(32, runner, agent, store);
    let connection_id = ConnectionId::new(3);

    processor
        .process_message(connection_id, initialize_request(1))
        .await;
    let _ = expect_response(&mut outgoing).await;
    processor
        .process_message(connection_id, initialized_notification())
        .await;
    processor
        .process_message(
            connection_id,
            request(2, "thread/start", json!({"agentKey": "default"})),
        )
        .await;
    let thread: ThreadStartResponse = decode_response(expect_response(&mut outgoing).await);
    let _ = next_notification_matching(&mut outgoing, |notification| {
        matches!(notification, ServerNotification::ThreadStarted(_))
    })
    .await;
    processor
        .process_message(
            connection_id,
            request(
                3,
                "turn/start",
                json!({
                    "threadId": thread.thread_id,
                    "input": [{"type": "text", "text": "start and then cancel"}]
                }),
            ),
        )
        .await;
    let started: TurnStartResponse = decode_response(expect_response(&mut outgoing).await);
    let _ = next_notification_matching(&mut outgoing, |notification| {
        matches!(notification, ServerNotification::ApprovalRequested(_))
    })
    .await;
    let _approval_request = next_server_request(&mut outgoing).await;

    processor
        .process_message(
            connection_id,
            request(
                4,
                "turn/interrupt",
                json!({
                    "threadId": thread.thread_id,
                    "expectedTurnId": started.turn_id,
                    "reason": "stop"
                }),
            ),
        )
        .await;
    let interrupt = loop {
        let response = expect_response(&mut outgoing).await;
        if response.id == RequestId::Integer(4) {
            break response;
        }
    };
    assert_eq!(interrupt.result["cancelled"], true);

    let completed = next_notification_matching(&mut outgoing, |notification| {
        matches!(notification, ServerNotification::TurnCompleted(_))
    })
    .await;
    let ServerNotification::TurnCompleted(completed) = completed else {
        unreachable!("matched turn completion")
    };
    assert_eq!(
        serde_json::to_value(completed.status).expect("turn status"),
        expected["turnStatus"]
    );
    assert_eq!(
        completed.completion_reason.as_deref(),
        expected["completionReason"].as_str()
    );
    assert!(completed
        .error
        .as_deref()
        .is_some_and(|error| error.contains("cancel")));

    processor
        .process_message(
            connection_id,
            request(5, "thread/read", json!({"threadId": thread.thread_id})),
        )
        .await;
    let read: ThreadReadResponse = decode_response(expect_response(&mut outgoing).await);
    let persisted = read
        .turns
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .expect("persisted cancelled turn");
    assert_eq!(
        serde_json::to_value(persisted.status).expect("persisted turn status"),
        expected["turnStatus"]
    );
    assert_eq!(
        persisted.result["completionReason"],
        expected["completionReason"]
    );
    assert_eq!(
        persisted.result.contains_key("error"),
        expected["errorField"] == "present"
    );
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

async fn next_server_request(rx: &mut mpsc::Receiver<OutgoingEnvelope>) -> JsonRpcRequest {
    loop {
        let envelope = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("message timeout")
            .expect("outgoing message");
        if let JsonRpcMessage::Request(request) = envelope.message {
            return request;
        }
    }
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
