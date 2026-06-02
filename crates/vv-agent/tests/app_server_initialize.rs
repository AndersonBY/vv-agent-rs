use std::time::Duration;

use serde_json::json;
use vv_agent::app_server::outgoing::OutgoingEnvelope;
use vv_agent::app_server::processor::MessageProcessor;
use vv_agent::app_server::protocol::{
    AppItem, AppItemKind, AppItemStatus, AppThread, JsonRpcMessage, JsonRpcNotification,
    JsonRpcRequest, RequestId, ServerNotification, ThreadStartedParams, ThreadStatus,
};
use vv_agent::app_server::transport::ConnectionId;

fn request(id: i64, method: &str, params: serde_json::Value) -> JsonRpcMessage {
    JsonRpcMessage::Request(JsonRpcRequest {
        id: RequestId::Integer(id),
        method: method.to_string(),
        params: Some(params),
    })
}

fn initialize_request(id: i64, opt_out: Vec<&str>) -> JsonRpcMessage {
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
                "optOutNotificationMethods": opt_out
            }
        }),
    )
}

async fn recv_message(rx: &mut tokio::sync::mpsc::Receiver<OutgoingEnvelope>) -> JsonRpcMessage {
    rx.recv().await.expect("outgoing").message
}

#[tokio::test]
async fn thread_start_before_initialize_returns_not_initialized() {
    let (mut processor, mut outgoing) = MessageProcessor::new_for_tests(8);
    let connection_id = ConnectionId::new(1);

    processor
        .process_message(connection_id, request(1, "thread/start", json!({})))
        .await;

    let JsonRpcMessage::Error(error) = recv_message(&mut outgoing).await else {
        panic!("expected error");
    };
    assert_eq!(error.error.code, -32010);
}

#[tokio::test]
async fn first_initialize_returns_capabilities() {
    let (mut processor, mut outgoing) = MessageProcessor::new_for_tests(8);
    let connection_id = ConnectionId::new(1);

    processor
        .process_message(connection_id, initialize_request(1, vec![]))
        .await;

    let JsonRpcMessage::Response(response) = recv_message(&mut outgoing).await else {
        panic!("expected response");
    };
    assert_eq!(response.id, RequestId::Integer(1));
    assert_eq!(response.result["protocolVersion"], "2026-06-02");
    assert_eq!(response.result["capabilities"]["thread"], true);
}

#[tokio::test]
async fn repeated_initialize_returns_already_initialized() {
    let (mut processor, mut outgoing) = MessageProcessor::new_for_tests(8);
    let connection_id = ConnectionId::new(1);

    processor
        .process_message(connection_id, initialize_request(1, vec![]))
        .await;
    let _ = recv_message(&mut outgoing).await;
    processor
        .process_message(connection_id, initialize_request(2, vec![]))
        .await;

    let JsonRpcMessage::Error(error) = recv_message(&mut outgoing).await else {
        panic!("expected error");
    };
    assert_eq!(error.error.code, -32011);
}

#[tokio::test]
async fn initialized_notification_marks_connection_ready_for_notifications() {
    let (mut processor, mut outgoing) = MessageProcessor::new_for_tests(8);
    let connection_id = ConnectionId::new(1);
    processor
        .process_message(connection_id, initialize_request(1, vec![]))
        .await;
    let _ = recv_message(&mut outgoing).await;

    processor
        .outgoing()
        .send_notification(connection_id, thread_started_notification())
        .await
        .expect("send suppressed notification");
    assert!(outgoing.try_recv().is_err());

    processor
        .process_message(
            connection_id,
            JsonRpcMessage::Notification(JsonRpcNotification {
                method: "initialized".to_string(),
                params: None,
            }),
        )
        .await;
    assert!(processor
        .connection_state(connection_id)
        .expect("state")
        .ready_for_notifications());

    processor
        .outgoing()
        .send_notification(connection_id, thread_started_notification())
        .await
        .expect("send notification");
    assert!(matches!(
        recv_message(&mut outgoing).await,
        JsonRpcMessage::Notification(_)
    ));
}

#[tokio::test]
async fn notification_opt_out_suppresses_exact_method_names() {
    let (mut processor, mut outgoing) = MessageProcessor::new_for_tests(8);
    let connection_id = ConnectionId::new(1);
    processor
        .process_message(connection_id, initialize_request(1, vec!["thread/started"]))
        .await;
    let _ = recv_message(&mut outgoing).await;
    processor
        .process_message(
            connection_id,
            JsonRpcMessage::Notification(JsonRpcNotification {
                method: "initialized".to_string(),
                params: None,
            }),
        )
        .await;

    processor
        .outgoing()
        .send_notification(connection_id, thread_started_notification())
        .await
        .expect("suppressed");
    assert!(
        tokio::time::timeout(Duration::from_millis(30), outgoing.recv())
            .await
            .is_err()
    );

    processor
        .outgoing()
        .send_notification(
            connection_id,
            ServerNotification::ItemStarted(vv_agent::app_server::protocol::ItemStartedParams {
                thread_id: "thread_1".to_string(),
                turn_id: "turn_1".to_string(),
                item: AppItem {
                    id: "item_1".to_string(),
                    run_event_id: "evt_1".to_string(),
                    kind: AppItemKind::RunStatus,
                    status: AppItemStatus::Completed,
                    created_at_ms: 1,
                    completed_at_ms: Some(1),
                    content: None,
                },
            }),
        )
        .await
        .expect("not suppressed");
    assert!(matches!(
        recv_message(&mut outgoing).await,
        JsonRpcMessage::Notification(_)
    ));
}

fn thread_started_notification() -> ServerNotification {
    ServerNotification::ThreadStarted(ThreadStartedParams {
        thread: AppThread {
            id: "thread_1".to_string(),
            title: None,
            cwd: None,
            model: None,
            status: ThreadStatus::Idle,
            archived: false,
            ephemeral: false,
            created_at_ms: 1,
            updated_at_ms: 1,
            active_turn_id: None,
        },
    })
}
