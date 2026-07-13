use std::collections::HashSet;
use std::io::{Cursor, Write};
use std::sync::{Arc, Mutex};

use serde_json::json;
use tokio::sync::mpsc;

use vv_agent::app_server::processor::MessageProcessor;
use vv_agent::app_server::protocol::{
    AppServerError, AppServerErrorCode, ApprovalRequestParams, JsonRpcMessage, JsonRpcRequest,
    JsonRpcResponse, RequestId, ServerRequest,
};
use vv_agent::app_server::transport::channel::ChannelTransport;
use vv_agent::app_server::transport::stdio::StdioJsonlTransport;
use vv_agent::app_server::transport::{
    AppServerTransport, ConnectionId, TransportConnectionMode, TransportEvent, TransportFuture,
};
use vv_agent::app_server::AppServer;

fn initialize_request() -> JsonRpcMessage {
    JsonRpcMessage::Request(JsonRpcRequest {
        id: RequestId::Integer(1),
        method: "initialize".to_string(),
        params: Some(json!({"clientInfo": {"name": "generic-server-test"}})),
    })
}

fn approval_request() -> ServerRequest {
    ServerRequest::ApprovalRequest(ApprovalRequestParams {
        thread_id: "thread_1".to_string(),
        turn_id: "turn_1".to_string(),
        request_id: "approval_1".to_string(),
        tool_call_id: "call_1".to_string(),
        tool_name: "test_tool".to_string(),
        preview: "test_tool {}".to_string(),
        arguments: json!({}),
    })
}

#[tokio::test]
async fn generic_server_handles_channel_lifecycle_and_async_outgoing() {
    let (transport, mut client) = ChannelTransport::pair(16);
    let connection_id = client.connection_id();
    let (processor, outgoing) = MessageProcessor::new(16);
    let outgoing_sender = processor.outgoing().clone();
    let mut server = AppServer::new(transport, processor, outgoing);

    let client_flow = async move {
        client.open().await.expect("open connection");
        client
            .send_message(initialize_request())
            .await
            .expect("send initialize");
        let JsonRpcMessage::Response(response) =
            client.recv_message().await.expect("initialize response")
        else {
            panic!("expected initialize response");
        };
        assert_eq!(response.id, RequestId::Integer(1));

        let (_request_id, callback) = outgoing_sender
            .send_server_request(connection_id, approval_request())
            .await
            .expect("queue asynchronous outgoing request");
        let JsonRpcMessage::Request(request) =
            client.recv_message().await.expect("asynchronous outgoing")
        else {
            panic!("expected server request");
        };
        assert_eq!(request.method, "approval/request");

        client.close().await.expect("close connection");
        let error = callback
            .await
            .expect("disconnect callback")
            .expect_err("closed connection must reject pending request");
        assert_eq!(error.message, "client_disconnected");
        drop(client);
    };

    let (server_result, ()) = tokio::join!(server.run(), client_flow);
    server_result.expect("server lifecycle");
}

#[tokio::test]
async fn single_connection_send_failure_disconnects_before_server_returns_error() {
    let connection_id = ConnectionId::new(1);
    let (mut processor, mut outgoing) = MessageProcessor::new(16);
    processor
        .process_message(connection_id, initialize_request())
        .await;
    let _ = outgoing.recv().await.expect("initialize response");
    let outgoing_sender = processor.outgoing().clone();
    let (_request_id, callback) = outgoing_sender
        .send_server_request(connection_id, approval_request())
        .await
        .expect("queue pending request");
    let mut server = AppServer::new(FailingSingleTransport, processor, outgoing);

    let error = server.run().await.expect_err("send must fail");
    let callback_error = callback
        .await
        .expect("disconnect callback")
        .expect_err("pending request must fail");

    assert_eq!(error.code(), AppServerErrorCode::ServerOverloaded);
    assert_eq!(callback_error.message, "client_disconnected");
    assert_eq!(outgoing_sender.pending_server_request_count().await, 0);
    assert!(
        !outgoing_sender
            .is_connection_registered(connection_id)
            .await
    );
    assert!(server.processor().connection_state(connection_id).is_none());
}

#[tokio::test]
async fn multi_connection_send_failure_disconnects_only_failed_connection() {
    let failed = ConnectionId::new(1);
    let healthy = ConnectionId::new(2);
    let (mut processor, mut outgoing) = MessageProcessor::new(16);
    for connection_id in [failed, healthy] {
        processor
            .process_message(connection_id, initialize_request())
            .await;
        let _ = outgoing.recv().await.expect("initialize response");
    }
    let outgoing_sender = processor.outgoing().clone();
    let (_failed_request_id, failed_callback) = outgoing_sender
        .send_server_request(failed, approval_request())
        .await
        .expect("failed connection request");
    let (healthy_request_id, healthy_callback) = outgoing_sender
        .send_server_request(healthy, approval_request())
        .await
        .expect("healthy connection request");
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel();
    let failing_connections = Arc::new(Mutex::new(HashSet::from([failed])));
    let transport = ControlledMultiTransport {
        event_rx,
        outbound_tx,
        failing_connections,
    };
    let mut server = AppServer::new(transport, processor, outgoing);
    let client_flow = async move {
        let failed_error = failed_callback
            .await
            .expect("failed callback")
            .expect_err("failed connection must disconnect");
        assert_eq!(failed_error.message, "client_disconnected");
        assert!(!outgoing_sender.is_connection_registered(failed).await);
        assert!(outgoing_sender.is_connection_registered(healthy).await);

        let (connection_id, message) = outbound_rx.recv().await.expect("healthy outbound");
        assert_eq!(connection_id, healthy);
        let JsonRpcMessage::Request(request) = message else {
            panic!("expected healthy server request");
        };
        assert_eq!(request.id, healthy_request_id);
        event_tx
            .send(TransportEvent::Message {
                connection_id: healthy,
                message: JsonRpcMessage::Response(JsonRpcResponse {
                    id: request.id,
                    result: json!({"decision": "allow"}),
                }),
            })
            .expect("send healthy response");
        assert_eq!(
            healthy_callback
                .await
                .expect("healthy callback")
                .expect("healthy result"),
            json!({"decision": "allow"})
        );
        drop(event_tx);
    };

    let (server_result, ()) = tokio::join!(server.run(), client_flow);

    server_result.expect("multi-connection server continues after one send failure");
    assert!(server.processor().connection_state(failed).is_none());
    assert!(server.processor().connection_state(healthy).is_none());
}

#[tokio::test]
async fn generic_server_accepts_stdio_transport_and_keeps_processing_after_parse_error() {
    let input = Cursor::new(
        b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"clientInfo\":{\"name\":\"stdio\"}}}\n{bad json}\n[]\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"model/list\"}\n"
            .to_vec(),
    );
    let output = SharedWriter::default();
    let output_view = output.clone();
    let transport = StdioJsonlTransport::from_io(ConnectionId::new(1), input, output);
    let (processor, outgoing) = MessageProcessor::new(16);
    let mut server = AppServer::new(transport, processor, outgoing);

    server.run().await.expect("stdio server");

    let messages = output_view
        .text()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("jsonl message"))
        .collect::<Vec<_>>();
    assert_eq!(messages[0]["id"], 1);
    assert_eq!(messages[0]["result"]["protocolVersion"], "v1");
    assert_eq!(messages[1]["error"]["code"], -32700);
    assert_eq!(messages[1]["error"]["message"], "Parse error");
    assert_eq!(messages[2]["error"]["code"], -32600);
    assert_eq!(messages[2]["error"]["message"], "Invalid Request");
    assert_eq!(messages[3]["id"], 2);
}

struct FailingSingleTransport;

impl AppServerTransport for FailingSingleTransport {
    fn next_event(
        &mut self,
    ) -> TransportFuture<'_, Option<Result<TransportEvent, AppServerError>>> {
        Box::pin(std::future::pending())
    }

    fn send(
        &self,
        _connection_id: ConnectionId,
        _message: JsonRpcMessage,
    ) -> TransportFuture<'_, Result<(), AppServerError>> {
        Box::pin(async { Err(AppServerError::server_overloaded()) })
    }
}

struct ControlledMultiTransport {
    event_rx: mpsc::UnboundedReceiver<TransportEvent>,
    outbound_tx: mpsc::UnboundedSender<(ConnectionId, JsonRpcMessage)>,
    failing_connections: Arc<Mutex<HashSet<ConnectionId>>>,
}

impl AppServerTransport for ControlledMultiTransport {
    fn next_event(
        &mut self,
    ) -> TransportFuture<'_, Option<Result<TransportEvent, AppServerError>>> {
        Box::pin(async move { self.event_rx.recv().await.map(Ok) })
    }

    fn send(
        &self,
        connection_id: ConnectionId,
        message: JsonRpcMessage,
    ) -> TransportFuture<'_, Result<(), AppServerError>> {
        let should_fail = self
            .failing_connections
            .lock()
            .expect("failure set")
            .contains(&connection_id);
        let outbound_tx = self.outbound_tx.clone();
        Box::pin(async move {
            if should_fail {
                return Err(AppServerError::server_overloaded());
            }
            outbound_tx
                .send((connection_id, message))
                .map_err(|_| AppServerError::internal("controlled transport closed"))
        })
    }

    fn connection_mode(&self) -> TransportConnectionMode {
        TransportConnectionMode::Multiple
    }
}

#[derive(Clone, Default)]
struct SharedWriter(Arc<Mutex<Vec<u8>>>);

impl SharedWriter {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().expect("writer").clone()).expect("utf-8")
    }
}

impl Write for SharedWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("writer").extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
