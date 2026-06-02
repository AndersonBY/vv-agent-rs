use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::app_server::protocol::{
    AppServerError, JsonRpcError, JsonRpcErrorBody, JsonRpcMessage, JsonRpcNotification,
    JsonRpcResponse, RequestId, ServerNotification, ServerRequest,
};
use crate::app_server::transport::ConnectionId;

pub type ServerRequestResult = Result<Value, JsonRpcErrorBody>;

#[derive(Debug, Clone, PartialEq)]
pub struct OutgoingEnvelope {
    pub connection_id: ConnectionId,
    pub message: JsonRpcMessage,
}

#[derive(Debug, Clone, Default)]
struct ConnectionOutgoingState {
    ready_for_notifications: bool,
    opt_out_notification_methods: HashSet<String>,
}

#[derive(Clone)]
pub struct OutgoingMessageSender {
    tx: mpsc::Sender<OutgoingEnvelope>,
    connections: Arc<Mutex<HashMap<ConnectionId, ConnectionOutgoingState>>>,
    pending_server_requests: Arc<Mutex<HashMap<RequestId, oneshot::Sender<ServerRequestResult>>>>,
    next_server_request_id: Arc<AtomicI64>,
}

impl OutgoingMessageSender {
    pub fn channel(capacity: usize) -> (Self, mpsc::Receiver<OutgoingEnvelope>) {
        let (tx, rx) = mpsc::channel(capacity);
        (
            Self {
                tx,
                connections: Arc::new(Mutex::new(HashMap::new())),
                pending_server_requests: Arc::new(Mutex::new(HashMap::new())),
                next_server_request_id: Arc::new(AtomicI64::new(1)),
            },
            rx,
        )
    }

    pub async fn register_connection(&self, connection_id: ConnectionId) {
        self.connections
            .lock()
            .await
            .entry(connection_id)
            .or_default();
    }

    pub async fn configure_connection(
        &self,
        connection_id: ConnectionId,
        opt_out_notification_methods: HashSet<String>,
    ) {
        let mut connections = self.connections.lock().await;
        let state = connections.entry(connection_id).or_default();
        state.opt_out_notification_methods = opt_out_notification_methods;
    }

    pub async fn mark_ready_for_notifications(&self, connection_id: ConnectionId) {
        self.connections
            .lock()
            .await
            .entry(connection_id)
            .or_default()
            .ready_for_notifications = true;
    }

    pub async fn send_response(
        &self,
        connection_id: ConnectionId,
        id: RequestId,
        result: Value,
    ) -> Result<(), AppServerError> {
        self.send_message(
            connection_id,
            JsonRpcMessage::Response(JsonRpcResponse { id, result }),
        )
        .await
    }

    pub async fn send_error(
        &self,
        connection_id: ConnectionId,
        id: RequestId,
        error: AppServerError,
    ) -> Result<(), AppServerError> {
        self.send_message(
            connection_id,
            JsonRpcMessage::Error(error.into_json_rpc_error(id)),
        )
        .await
    }

    pub async fn send_notification(
        &self,
        connection_id: ConnectionId,
        notification: ServerNotification,
    ) -> Result<(), AppServerError> {
        if !self
            .should_send_notification(connection_id, &notification)
            .await
        {
            return Ok(());
        }
        let message = server_notification_message(notification)?;
        self.send_message(connection_id, message).await
    }

    pub async fn broadcast_initialized_client_notification(
        &self,
        notification: ServerNotification,
    ) -> Result<(), AppServerError> {
        let connection_ids: Vec<ConnectionId> = {
            self.connections
                .lock()
                .await
                .iter()
                .filter_map(|(connection_id, state)| {
                    if state.ready_for_notifications
                        && !state
                            .opt_out_notification_methods
                            .contains(notification_method(&notification))
                    {
                        Some(*connection_id)
                    } else {
                        None
                    }
                })
                .collect()
        };

        for connection_id in connection_ids {
            self.send_notification(connection_id, notification.clone())
                .await?;
        }
        Ok(())
    }

    pub async fn send_server_request(
        &self,
        connection_id: ConnectionId,
        request: ServerRequest,
    ) -> Result<(RequestId, oneshot::Receiver<ServerRequestResult>), AppServerError> {
        let id = RequestId::String(format!(
            "srvreq_{}",
            self.next_server_request_id.fetch_add(1, Ordering::Relaxed)
        ));
        let (callback_tx, callback_rx) = oneshot::channel();
        self.pending_server_requests
            .lock()
            .await
            .insert(id.clone(), callback_tx);
        let (method, params) = tagged_method_params(&request)?;
        self.send_message(
            connection_id,
            JsonRpcMessage::Request(crate::app_server::protocol::JsonRpcRequest {
                id: id.clone(),
                method,
                params,
            }),
        )
        .await?;
        Ok((id, callback_rx))
    }

    pub async fn resolve_server_response(&self, response: JsonRpcResponse) -> bool {
        self.pending_server_requests
            .lock()
            .await
            .remove(&response.id)
            .is_some_and(|callback| callback.send(Ok(response.result)).is_ok())
    }

    pub async fn resolve_server_error(&self, error: JsonRpcError) -> bool {
        self.pending_server_requests
            .lock()
            .await
            .remove(&error.id)
            .is_some_and(|callback| callback.send(Err(error.error)).is_ok())
    }

    async fn send_message(
        &self,
        connection_id: ConnectionId,
        message: JsonRpcMessage,
    ) -> Result<(), AppServerError> {
        self.tx
            .send(OutgoingEnvelope {
                connection_id,
                message,
            })
            .await
            .map_err(|_| AppServerError::internal("outgoing channel closed"))
    }

    async fn should_send_notification(
        &self,
        connection_id: ConnectionId,
        notification: &ServerNotification,
    ) -> bool {
        self.connections
            .lock()
            .await
            .get(&connection_id)
            .is_some_and(|state| {
                state.ready_for_notifications
                    && !state
                        .opt_out_notification_methods
                        .contains(notification_method(notification))
            })
    }
}

fn server_notification_message(
    notification: ServerNotification,
) -> Result<JsonRpcMessage, AppServerError> {
    let (method, params) = tagged_method_params(&notification)?;
    Ok(JsonRpcMessage::Notification(JsonRpcNotification {
        method,
        params,
    }))
}

fn tagged_method_params<T: Serialize>(
    value: &T,
) -> Result<(String, Option<Value>), AppServerError> {
    let mut value =
        serde_json::to_value(value).map_err(|error| AppServerError::internal(error.to_string()))?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| AppServerError::internal("tagged protocol value is not an object"))?;
    let method = object
        .remove("method")
        .and_then(|method| method.as_str().map(str::to_string))
        .ok_or_else(|| AppServerError::internal("tagged protocol value is missing method"))?;
    let params = object.remove("params");
    Ok((method, params))
}

fn notification_method(notification: &ServerNotification) -> &'static str {
    match notification {
        ServerNotification::ThreadStarted(_) => "thread/started",
        ServerNotification::ThreadArchived(_) => "thread/archived",
        ServerNotification::TurnStarted(_) => "turn/started",
        ServerNotification::TurnCompleted(_) => "turn/completed",
        ServerNotification::ItemStarted(_) => "item/started",
        ServerNotification::AgentMessageDelta(_) => "item/agentMessage/delta",
        ServerNotification::ToolCallDelta(_) => "item/toolCall/delta",
        ServerNotification::ItemCompleted(_) => "item/completed",
        ServerNotification::ApprovalRequested(_) => "approval/requested",
        ServerNotification::ApprovalResolved(_) => "approval/resolved",
        ServerNotification::ErrorWarning(_) => "error/warning",
    }
}
