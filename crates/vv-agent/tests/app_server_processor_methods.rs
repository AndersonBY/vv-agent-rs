use serde_json::json;
use std::time::Duration;
use vv_agent::app_server::outgoing::OutgoingEnvelope;
use vv_agent::app_server::processor::MessageProcessor;
use vv_agent::app_server::protocol::{
    ApprovalDecision, ApprovalRequestParams, JsonRpcMessage, JsonRpcRequest, RequestId,
    ServerRequest,
};
use vv_agent::app_server::transport::ConnectionId;

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
            "clientInfo": {"name": "processor-method-test"},
            "capabilities": {}
        }),
    )
}

async fn initialize(
    processor: &mut MessageProcessor,
    outgoing: &mut tokio::sync::mpsc::Receiver<OutgoingEnvelope>,
    connection_id: ConnectionId,
) {
    processor
        .process_message(connection_id, initialize_request(1))
        .await;
    let _ = outgoing.recv().await.expect("initialize response");
}

async fn recv_for_connection(
    outgoing: &mut tokio::sync::mpsc::Receiver<OutgoingEnvelope>,
    connection_id: ConnectionId,
) -> JsonRpcMessage {
    loop {
        let envelope = outgoing.recv().await.expect("outgoing message");
        if envelope.connection_id == connection_id {
            return envelope.message;
        }
    }
}

#[tokio::test]
async fn schema_export_request_returns_json_and_typescript_bundles() {
    let (mut processor, mut outgoing) = MessageProcessor::new_for_tests(16);
    let connection_id = ConnectionId::new(1);
    initialize(&mut processor, &mut outgoing, connection_id).await;

    processor
        .process_message(connection_id, request(2, "schema/export", json!({})))
        .await;

    let JsonRpcMessage::Response(response) =
        recv_for_connection(&mut outgoing, connection_id).await
    else {
        panic!("expected schema export response");
    };
    assert_eq!(response.id, RequestId::Integer(2));
    assert!(response.result["jsonSchema"]["ClientRequest"].is_string());
    assert!(response.result["jsonSchema"]["ServerNotification"].is_string());
    assert!(response.result["typescript"]["ClientRequest.ts"].is_string());
}

#[tokio::test]
async fn model_list_request_returns_a_valid_model_list_response() {
    let (mut processor, mut outgoing) = MessageProcessor::new_for_tests(16);
    let connection_id = ConnectionId::new(1);
    initialize(&mut processor, &mut outgoing, connection_id).await;

    processor
        .process_message(connection_id, request(2, "model/list", json!({})))
        .await;

    let JsonRpcMessage::Response(response) =
        recv_for_connection(&mut outgoing, connection_id).await
    else {
        panic!("expected model list response");
    };
    assert_eq!(response.id, RequestId::Integer(2));
    assert!(response.result["models"].is_array());
}

#[tokio::test]
async fn approval_resolve_request_resolves_matching_server_request() {
    let (mut processor, mut outgoing) = MessageProcessor::new_for_tests(16);
    let connection_id = ConnectionId::new(1);
    initialize(&mut processor, &mut outgoing, connection_id).await;

    let callback = processor
        .outgoing()
        .send_server_request_with_id(
            connection_id,
            RequestId::String("approval_1".to_string()),
            ServerRequest::ApprovalRequest(ApprovalRequestParams {
                thread_id: "thread_1".to_string(),
                turn_id: "turn_1".to_string(),
                request_id: "approval_1".to_string(),
                tool_name: "bash".to_string(),
                preview: "Run cargo test".to_string(),
                choices: vec![ApprovalDecision::Allow, ApprovalDecision::Deny],
            }),
        )
        .await
        .expect("server request");
    let _ = recv_for_connection(&mut outgoing, connection_id).await;

    processor
        .process_message(
            connection_id,
            request(
                2,
                "approval/resolve",
                json!({
                    "threadId": "thread_1",
                    "turnId": "turn_1",
                    "requestId": "approval_1",
                    "decision": "allow"
                }),
            ),
        )
        .await;

    let result = tokio::time::timeout(Duration::from_millis(100), callback)
        .await
        .expect("approval callback should resolve")
        .expect("callback")
        .expect("approval result");
    assert_eq!(result["decision"], "allow");
    let JsonRpcMessage::Response(response) =
        recv_for_connection(&mut outgoing, connection_id).await
    else {
        panic!("expected approval resolve response");
    };
    assert_eq!(response.id, RequestId::Integer(2));
}

#[tokio::test]
async fn turn_steer_returns_explicit_unsupported_error() {
    let (mut processor, mut outgoing) = MessageProcessor::new_for_tests(16);
    let connection_id = ConnectionId::new(1);
    initialize(&mut processor, &mut outgoing, connection_id).await;

    processor
        .process_message(
            connection_id,
            request(
                2,
                "turn/steer",
                json!({
                    "threadId": "thread_1",
                    "turnId": "turn_1",
                    "input": [{"text": "continue"}]
                }),
            ),
        )
        .await;

    let JsonRpcMessage::Error(error) = recv_for_connection(&mut outgoing, connection_id).await
    else {
        panic!("expected unsupported error");
    };
    assert_eq!(error.id, RequestId::Integer(2));
    assert_eq!(error.error.code, -32013);
    assert_eq!(error.error.data.expect("data")["method"], "turn/steer");
}
