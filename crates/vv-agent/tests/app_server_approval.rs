use std::time::Duration;

use serde_json::json;
use tokio::sync::mpsc;
use vv_agent::app_server::outgoing::OutgoingEnvelope;
use vv_agent::app_server::outgoing::OutgoingMessageSender;
use vv_agent::app_server::processor::MessageProcessor;
use vv_agent::app_server::protocol::{
    ApprovalDecision, ApprovalRequestParams, JsonRpcError, JsonRpcErrorBody, JsonRpcMessage,
    JsonRpcNotification, JsonRpcRequest, JsonRpcResponse, RequestId, ServerNotification,
    ServerRequest, ThreadStartResponse, TurnStartResponse,
};
use vv_agent::app_server::thread_store::SqliteThreadStore;
use vv_agent::app_server::transport::ConnectionId;
use vv_agent::{
    Agent, FunctionTool, LLMResponse, ModelRef, Runner, ScriptedModelProvider, ToolCall, ToolOutput,
};

fn approval_request() -> ServerRequest {
    ServerRequest::ApprovalRequest(ApprovalRequestParams {
        thread_id: "thread_1".to_string(),
        turn_id: "turn_1".to_string(),
        request_id: "approval_1".to_string(),
        tool_name: "dangerous".to_string(),
        preview: "dangerous {}".to_string(),
        choices: vec![ApprovalDecision::Allow, ApprovalDecision::Deny],
    })
}

#[tokio::test]
async fn callback_server_request_emits_json_rpc_request_with_string_id() {
    let (outgoing, mut rx) = OutgoingMessageSender::channel(8);
    let connection_id = ConnectionId::new(1);
    outgoing.register_connection(connection_id).await;

    let (request_id, _callback) = outgoing
        .send_server_request(connection_id, approval_request())
        .await
        .expect("server request");

    let envelope = rx.recv().await.expect("outgoing");
    assert_eq!(envelope.connection_id, connection_id);
    let JsonRpcMessage::Request(request) = envelope.message else {
        panic!("expected json-rpc request");
    };
    assert_eq!(request.id, request_id);
    assert!(matches!(request.id, RequestId::String(_)));
    assert_eq!(request.method, "approval/request");
    assert_eq!(request.params.expect("params")["requestId"], "approval_1");
}

#[tokio::test]
async fn callback_client_response_resolves_pending_request() {
    let (outgoing, mut rx) = OutgoingMessageSender::channel(8);
    let connection_id = ConnectionId::new(1);
    outgoing.register_connection(connection_id).await;
    let (request_id, callback) = outgoing
        .send_server_request(connection_id, approval_request())
        .await
        .expect("server request");
    let _ = rx.recv().await.expect("outgoing request");

    assert!(
        outgoing
            .resolve_server_response(JsonRpcResponse {
                id: request_id,
                result: json!({"decision": "allow"}),
            })
            .await
    );

    let result = callback.await.expect("callback").expect("result");
    assert_eq!(result["decision"], "allow");
}

#[tokio::test]
async fn callback_client_error_resolves_pending_request_with_error() {
    let (outgoing, mut rx) = OutgoingMessageSender::channel(8);
    let connection_id = ConnectionId::new(1);
    outgoing.register_connection(connection_id).await;
    let (request_id, callback) = outgoing
        .send_server_request(connection_id, approval_request())
        .await
        .expect("server request");
    let _ = rx.recv().await.expect("outgoing request");

    assert!(
        outgoing
            .resolve_server_error(JsonRpcError {
                id: request_id,
                error: JsonRpcErrorBody {
                    code: -32603,
                    message: "client failed".to_string(),
                    data: None,
                },
            })
            .await
    );

    let error = callback.await.expect("callback").expect_err("error");
    assert_eq!(error.message, "client failed");
}

#[tokio::test]
async fn callback_timeout_removes_pending_request() {
    let (outgoing, mut rx) = OutgoingMessageSender::channel(8);
    let connection_id = ConnectionId::new(1);
    outgoing.register_connection(connection_id).await;

    let result = outgoing
        .send_server_request_with_timeout(
            connection_id,
            approval_request(),
            Duration::from_millis(10),
        )
        .await;
    let JsonRpcMessage::Request(request) = rx.recv().await.expect("outgoing").message else {
        panic!("expected request");
    };

    let error = result.expect_err("timeout");
    assert_eq!(error.code().code(), -32603);
    assert_eq!(outgoing.pending_server_request_count().await, 0);
    assert!(
        !outgoing
            .resolve_server_response(JsonRpcResponse {
                id: request.id,
                result: json!({"decision": "allow"}),
            })
            .await
    );
}

#[tokio::test]
async fn callback_duplicate_response_is_ignored_after_first_resolution() {
    let (outgoing, mut rx) = OutgoingMessageSender::channel(8);
    let connection_id = ConnectionId::new(1);
    outgoing.register_connection(connection_id).await;
    let (request_id, callback) = outgoing
        .send_server_request(connection_id, approval_request())
        .await
        .expect("server request");
    let _ = rx.recv().await.expect("outgoing request");

    assert!(
        outgoing
            .resolve_server_response(JsonRpcResponse {
                id: request_id.clone(),
                result: json!({"decision": "allow"}),
            })
            .await
    );
    assert!(
        !outgoing
            .resolve_server_response(JsonRpcResponse {
                id: request_id,
                result: json!({"decision": "deny"}),
            })
            .await
    );
    let result = callback.await.expect("callback").expect("result");
    assert_eq!(result["decision"], "allow");
}

#[tokio::test]
async fn callback_disconnected_client_does_not_leave_pending_request() {
    let (outgoing, rx) = OutgoingMessageSender::channel(8);
    drop(rx);
    let connection_id = ConnectionId::new(1);
    outgoing.register_connection(connection_id).await;

    let error = outgoing
        .send_server_request(connection_id, approval_request())
        .await
        .expect_err("disconnected client");

    assert_eq!(error.message(), "outgoing channel closed");
    assert_eq!(outgoing.pending_server_request_count().await, 0);
}

#[tokio::test]
async fn approval_run_sends_server_request_and_allows_tool_after_client_response() {
    let tool_runs = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let (mut processor, mut outgoing) = approval_processor(tool_runs.clone());
    let connection_id = ConnectionId::new(1);
    initialize(&mut processor, &mut outgoing, connection_id).await;
    let thread_id = start_thread(&mut processor, &mut outgoing, connection_id).await;

    processor
        .process_message(
            connection_id,
            request(
                3,
                "turn/start",
                json!({
                    "threadId": thread_id,
                    "input": [{"text": "do it"}],
                    "model": "approval-model"
                }),
            ),
        )
        .await;
    let _turn = expect_response(&mut outgoing).await;

    let approval = next_notification_matching(&mut outgoing, |notification| {
        matches!(notification, ServerNotification::ApprovalRequested(_))
    })
    .await;
    let server_request = next_server_request(&mut outgoing).await;
    let ServerNotification::ApprovalRequested(approval) = approval else {
        unreachable!("matched approval")
    };
    assert_eq!(server_request.method, "approval/request");
    assert_eq!(
        server_request.params.as_ref().expect("params")["requestId"],
        approval.request_id
    );
    assert!(tool_runs.lock().expect("runs").is_empty());

    processor
        .process_message(
            connection_id,
            JsonRpcMessage::Response(JsonRpcResponse {
                id: server_request.id,
                result: json!({ "decision": "allow" }),
            }),
        )
        .await;

    let completed = next_notification_matching(&mut outgoing, |notification| {
        matches!(notification, ServerNotification::TurnCompleted(_))
    })
    .await;
    assert!(matches!(completed, ServerNotification::TurnCompleted(_)));
    assert_eq!(
        tool_runs.lock().expect("runs").as_slice(),
        &["dangerous ran".to_string()]
    );
}

#[tokio::test]
async fn approval_run_denies_tool_and_reports_denied_resolution() {
    let tool_runs = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let (mut processor, mut outgoing) = approval_processor(tool_runs.clone());
    let connection_id = ConnectionId::new(1);
    initialize(&mut processor, &mut outgoing, connection_id).await;
    let thread_id = start_thread(&mut processor, &mut outgoing, connection_id).await;

    processor
        .process_message(
            connection_id,
            request(
                3,
                "turn/start",
                json!({
                    "threadId": thread_id,
                    "input": [{"text": "do it"}],
                    "model": "approval-model"
                }),
            ),
        )
        .await;
    let _turn = expect_response(&mut outgoing).await;
    let _approval = next_notification_matching(&mut outgoing, |notification| {
        matches!(notification, ServerNotification::ApprovalRequested(_))
    })
    .await;
    let server_request = next_server_request(&mut outgoing).await;

    processor
        .process_message(
            connection_id,
            JsonRpcMessage::Response(JsonRpcResponse {
                id: server_request.id,
                result: json!({ "decision": "deny", "message": "not allowed" }),
            }),
        )
        .await;

    let resolved = next_notification_matching(&mut outgoing, |notification| {
        matches!(notification, ServerNotification::ApprovalResolved(_))
    })
    .await;
    let ServerNotification::ApprovalResolved(resolved) = resolved else {
        unreachable!("matched approval resolved")
    };
    assert_eq!(resolved.decision, ApprovalDecision::Deny);

    let _completed = next_notification_matching(&mut outgoing, |notification| {
        matches!(notification, ServerNotification::TurnCompleted(_))
    })
    .await;
    assert!(tool_runs.lock().expect("runs").is_empty());
}

#[tokio::test]
async fn approval_run_times_out_when_client_does_not_respond() {
    let tool_runs = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let (mut processor, mut outgoing) =
        approval_processor_with_timeout(tool_runs.clone(), Duration::from_millis(20));
    let connection_id = ConnectionId::new(1);
    initialize(&mut processor, &mut outgoing, connection_id).await;
    let thread_id = start_thread(&mut processor, &mut outgoing, connection_id).await;

    processor
        .process_message(
            connection_id,
            request(
                3,
                "turn/start",
                json!({
                    "threadId": thread_id,
                    "input": [{"text": "do it"}],
                    "model": "approval-model"
                }),
            ),
        )
        .await;
    let _turn = expect_response(&mut outgoing).await;
    let _approval = next_notification_matching(&mut outgoing, |notification| {
        matches!(notification, ServerNotification::ApprovalRequested(_))
    })
    .await;
    let _server_request = next_server_request(&mut outgoing).await;

    let resolved = next_notification_matching(&mut outgoing, |notification| {
        matches!(notification, ServerNotification::ApprovalResolved(_))
    })
    .await;
    let ServerNotification::ApprovalResolved(resolved) = resolved else {
        unreachable!("matched approval resolved")
    };
    assert_eq!(resolved.decision, ApprovalDecision::Deny);

    let _completed = next_notification_matching(&mut outgoing, |notification| {
        matches!(notification, ServerNotification::TurnCompleted(_))
    })
    .await;
    assert!(tool_runs.lock().expect("runs").is_empty());
}

#[tokio::test]
async fn approval_run_interrupt_releases_pending_approval_without_waiting_for_timeout() {
    let tool_runs = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let (mut processor, mut outgoing) =
        approval_processor_with_timeout(tool_runs.clone(), Duration::from_secs(30));
    let connection_id = ConnectionId::new(1);
    initialize(&mut processor, &mut outgoing, connection_id).await;
    let thread_id = start_thread(&mut processor, &mut outgoing, connection_id).await;

    processor
        .process_message(
            connection_id,
            request(
                3,
                "turn/start",
                json!({
                    "threadId": thread_id,
                    "input": [{"text": "do it"}],
                    "model": "approval-model"
                }),
            ),
        )
        .await;
    let turn: TurnStartResponse = decode_response(expect_response(&mut outgoing).await);
    let _approval = next_notification_matching(&mut outgoing, |notification| {
        matches!(notification, ServerNotification::ApprovalRequested(_))
    })
    .await;
    let _server_request = next_server_request(&mut outgoing).await;

    tokio::time::timeout(
        Duration::from_secs(3),
        processor.process_message(
            connection_id,
            request(
                4,
                "turn/interrupt",
                json!({
                    "threadId": thread_id,
                    "turnId": turn.turn.id
                }),
            ),
        ),
    )
    .await
    .expect("turn interrupt should not wait for approval timeout");
    let _interrupt = expect_response(&mut outgoing).await;

    let resolved = next_notification_matching(&mut outgoing, |notification| {
        matches!(notification, ServerNotification::ApprovalResolved(_))
    })
    .await;
    assert!(matches!(resolved, ServerNotification::ApprovalResolved(_)));
    let _completed = next_notification_matching(&mut outgoing, |notification| {
        matches!(notification, ServerNotification::TurnCompleted(_))
    })
    .await;
    assert!(tool_runs.lock().expect("runs").is_empty());
}

fn approval_processor(
    tool_runs: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
) -> (MessageProcessor, mpsc::Receiver<OutgoingEnvelope>) {
    approval_processor_with_timeout(tool_runs, Duration::from_secs(30))
}

fn approval_processor_with_timeout(
    tool_runs: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    timeout: Duration,
) -> (MessageProcessor, mpsc::Receiver<OutgoingEnvelope>) {
    let dangerous_runs = tool_runs.clone();
    let dangerous = FunctionTool::builder("dangerous")
        .description("Requires approval.")
        .json_schema(json!({"type":"object","properties":{},"required":[]}))
        .handler(move |_ctx, _args: serde_json::Value| {
            let dangerous_runs = dangerous_runs.clone();
            async move {
                dangerous_runs
                    .lock()
                    .expect("runs")
                    .push("dangerous ran".to_string());
                Ok(ToolOutput::text("allowed"))
            }
        })
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
    MessageProcessor::new_for_tests_with_runtime_and_approval_timeout(
        64,
        runner,
        agent,
        SqliteThreadStore::in_memory().expect("store"),
        timeout,
    )
}

async fn initialize(
    processor: &mut MessageProcessor,
    outgoing: &mut mpsc::Receiver<OutgoingEnvelope>,
    connection_id: ConnectionId,
) {
    processor
        .process_message(
            connection_id,
            request(
                1,
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
    let _ = expect_response(outgoing).await;
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
) -> String {
    processor
        .process_message(
            connection_id,
            request(
                2,
                "thread/start",
                json!({
                    "title": "approval",
                    "model": "approval-model",
                    "ephemeral": false
                }),
            ),
        )
        .await;
    let response: ThreadStartResponse = decode_response(expect_response(outgoing).await);
    let _ = expect_notification(outgoing).await;
    response.thread.id
}

fn request(id: i64, method: &str, params: serde_json::Value) -> JsonRpcMessage {
    JsonRpcMessage::Request(JsonRpcRequest {
        id: RequestId::Integer(id),
        method: method.to_string(),
        params: Some(params),
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
