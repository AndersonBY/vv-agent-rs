use std::path::Path;

use serde_json::json;
use vv_agent::app_server::protocol::{
    AppServerErrorCode, JsonRpcMessage, JsonRpcNotification, JsonRpcResponse, RequestId,
};
use vv_agent::app_server::transport::channel::ChannelTransport;
use vv_agent::app_server::transport::stdio::{parse_jsonl_message, serialize_jsonl_message};
use vv_agent::app_server::transport::{AppServerTransport, TransportEvent};

#[test]
fn jsonl_parser_decodes_one_message_per_line() {
    let message =
        parse_jsonl_message(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#)
            .expect("parse");

    assert!(matches!(
        message,
        Some(JsonRpcMessage::Request(request)) if request.id == RequestId::Integer(1)
            && request.method == "initialize"
    ));
}

#[test]
fn jsonl_parser_rejects_message_without_json_rpc_version() {
    let error = parse_jsonl_message(r#"{"id":1,"method":"initialize","params":{}}"#)
        .expect_err("missing jsonrpc version");

    assert_eq!(error.code(), AppServerErrorCode::InvalidRequest);
    assert_eq!(error.message(), "Invalid Request");
}

#[test]
fn jsonl_writer_emits_one_line_per_message() {
    let line = serialize_jsonl_message(&JsonRpcMessage::Response(JsonRpcResponse {
        id: RequestId::Integer(1),
        result: json!({"ok": true}),
    }))
    .expect("serialize");

    assert!(line.ends_with('\n'));
    assert_eq!(line.matches('\n').count(), 1);
    let decoded: JsonRpcMessage = serde_json::from_str(line.trim_end()).expect("decode");
    assert!(matches!(decoded, JsonRpcMessage::Response(_)));
}

#[test]
fn invalid_json_line_returns_parse_error() {
    let error = parse_jsonl_message("{not json").expect_err("invalid json");

    assert_eq!(error.code(), AppServerErrorCode::ParseError);
    assert_eq!(error.message(), "Parse error");
}

#[test]
fn empty_jsonl_line_is_ignored() {
    assert!(parse_jsonl_message("").expect("empty").is_none());
    assert!(parse_jsonl_message("   ").expect("blank").is_none());
}

#[tokio::test]
async fn channel_transport_can_send_request_and_read_response() {
    let (mut transport, mut client) = ChannelTransport::pair(4);
    client.open().await.expect("open");
    client
        .send_message(JsonRpcMessage::Notification(JsonRpcNotification {
            method: "initialized".to_string(),
            params: None,
        }))
        .await
        .expect("send");

    assert!(matches!(
        transport.next_event().await.expect("event").expect("ok"),
        TransportEvent::Opened { .. }
    ));
    let TransportEvent::Message {
        connection_id,
        message,
    } = transport.next_event().await.expect("event").expect("ok")
    else {
        panic!("expected message event");
    };
    assert!(matches!(message, JsonRpcMessage::Notification(_)));

    transport
        .send(
            connection_id,
            JsonRpcMessage::Response(JsonRpcResponse {
                id: RequestId::Integer(1),
                result: json!({"ok": true}),
            }),
        )
        .await
        .expect("response");

    assert!(matches!(
        client.recv_message().await.expect("outbound"),
        JsonRpcMessage::Response(_)
    ));
}

#[tokio::test]
async fn channel_transport_can_send_notification() {
    let (transport, mut client) = ChannelTransport::pair(4);
    let connection_id = client.connection_id();

    transport
        .send(
            connection_id,
            JsonRpcMessage::Notification(JsonRpcNotification {
                method: "thread/started".to_string(),
                params: Some(json!({"threadId": "thread_1"})),
            }),
        )
        .await
        .expect("send notification");

    let JsonRpcMessage::Notification(notification) =
        client.recv_message().await.expect("notification")
    else {
        panic!("expected notification");
    };
    assert_eq!(notification.method, "thread/started");
}

#[tokio::test]
async fn channel_transport_outbound_queue_full_returns_overload() {
    let (transport, mut client) = ChannelTransport::pair(1);
    let connection_id = client.connection_id();
    let message = JsonRpcMessage::Notification(JsonRpcNotification {
        method: "thread/started".to_string(),
        params: None,
    });

    transport
        .send(connection_id, message.clone())
        .await
        .expect("first send");
    let error = transport
        .send(connection_id, message)
        .await
        .expect_err("queue full");

    assert_eq!(error.code(), AppServerErrorCode::ServerOverloaded);
    assert!(client.recv_message().await.is_some());
}

#[test]
fn transport_modules_do_not_depend_on_runner() {
    let transport_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/app_server/transport");
    for entry in std::fs::read_dir(transport_dir).expect("transport dir") {
        let path = entry.expect("entry").path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("source");
        assert!(
            !source.contains("Runner"),
            "transport source must not depend on Runner: {}",
            path.display()
        );
    }
}
