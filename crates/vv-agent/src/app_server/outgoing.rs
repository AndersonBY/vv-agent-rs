use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::app_server::protocol::{
    AppServerError, AppServerErrorCode, JsonRpcError, JsonRpcErrorBody, JsonRpcMessage,
    JsonRpcNotification, JsonRpcResponse, RequestId, ServerNotification, ServerRequest,
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

#[derive(Debug)]
struct PendingServerRequest {
    connection_id: ConnectionId,
    method: String,
    thread_id: Option<String>,
    turn_id: Option<String>,
    callback: oneshot::Sender<ServerRequestResult>,
}

#[derive(Clone)]
pub struct OutgoingMessageSender {
    tx: mpsc::Sender<OutgoingEnvelope>,
    connections: Arc<Mutex<HashMap<ConnectionId, ConnectionOutgoingState>>>,
    pending_server_requests: Arc<Mutex<HashMap<RequestId, PendingServerRequest>>>,
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

    pub async fn is_connection_registered(&self, connection_id: ConnectionId) -> bool {
        self.connections.lock().await.contains_key(&connection_id)
    }

    pub async fn unregister_connection(&self, connection_id: ConnectionId) {
        self.connections.lock().await.remove(&connection_id);
        let pending = {
            let mut requests = self.pending_server_requests.lock().await;
            let request_ids = requests
                .iter()
                .filter_map(|(request_id, pending)| {
                    (pending.connection_id == connection_id).then_some(request_id.clone())
                })
                .collect::<Vec<_>>();
            request_ids
                .into_iter()
                .filter_map(|request_id| requests.remove(&request_id))
                .collect::<Vec<_>>()
        };
        let error = JsonRpcErrorBody {
            code: AppServerErrorCode::InternalError.code(),
            message: "client_disconnected".to_string(),
            data: None,
        };
        for pending in pending {
            let _ = pending.callback.send(Err(error.clone()));
        }
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
        let callback_rx = self
            .send_server_request_with_id(connection_id, id.clone(), request)
            .await?;
        Ok((id, callback_rx))
    }

    pub async fn send_server_request_with_timeout(
        &self,
        connection_id: ConnectionId,
        request: ServerRequest,
        timeout: std::time::Duration,
    ) -> Result<Value, AppServerError> {
        let (id, callback_rx) = self.send_server_request(connection_id, request).await?;
        match tokio::time::timeout(timeout, callback_rx).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(error))) => Err(AppServerError::new(
                AppServerErrorCode::InternalError,
                error.message,
            )
            .with_data(error.data.unwrap_or(Value::Null))),
            Ok(Err(_)) => Err(AppServerError::internal("server request callback dropped")),
            Err(_) => {
                self.pending_server_requests.lock().await.remove(&id);
                Err(AppServerError::internal("server request timed out"))
            }
        }
    }

    pub async fn pending_server_request_count(&self) -> usize {
        self.pending_server_requests.lock().await.len()
    }

    pub async fn send_server_request_with_id(
        &self,
        connection_id: ConnectionId,
        id: RequestId,
        request: ServerRequest,
    ) -> Result<oneshot::Receiver<ServerRequestResult>, AppServerError> {
        if matches!(id, RequestId::Null) {
            return Err(AppServerError::invalid_params(
                "Server request id cannot be null",
            ));
        }
        let (callback_tx, callback_rx) = oneshot::channel();
        let (method, params) = tagged_method_params(&request)?;
        let (thread_id, turn_id) = server_request_scope(&request);
        {
            let mut requests = self.pending_server_requests.lock().await;
            if requests.contains_key(&id) {
                return Err(AppServerError::invalid_params(
                    "Duplicate server request id",
                ));
            }
            requests.insert(
                id.clone(),
                PendingServerRequest {
                    connection_id,
                    method: method.clone(),
                    thread_id,
                    turn_id,
                    callback: callback_tx,
                },
            );
        }
        if let Err(error) = self
            .send_message(
                connection_id,
                JsonRpcMessage::Request(crate::app_server::protocol::JsonRpcRequest {
                    id: id.clone(),
                    method,
                    params,
                }),
            )
            .await
        {
            self.pending_server_requests.lock().await.remove(&id);
            return Err(error);
        }
        Ok(callback_rx)
    }

    pub async fn send_server_request_with_id_and_timeout(
        &self,
        connection_id: ConnectionId,
        id: RequestId,
        request: ServerRequest,
        timeout: std::time::Duration,
    ) -> Result<Value, AppServerError> {
        let callback_rx = self
            .send_server_request_with_id(connection_id, id.clone(), request)
            .await?;
        match tokio::time::timeout(timeout, callback_rx).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(error))) => Err(AppServerError::new(
                AppServerErrorCode::InternalError,
                error.message,
            )
            .with_data(error.data.unwrap_or(Value::Null))),
            Ok(Err(_)) => Err(AppServerError::internal("server request callback dropped")),
            Err(_) => {
                self.pending_server_requests.lock().await.remove(&id);
                Err(AppServerError::internal("server request timed out"))
            }
        }
    }

    pub async fn send_json_rpc_request(
        &self,
        connection_id: ConnectionId,
        id: RequestId,
        method: String,
        params: Option<Value>,
    ) -> Result<(), AppServerError> {
        self.send_message(
            connection_id,
            JsonRpcMessage::Request(crate::app_server::protocol::JsonRpcRequest {
                id,
                method,
                params,
            }),
        )
        .await
    }

    pub async fn resolve_server_response(
        &self,
        connection_id: ConnectionId,
        response: JsonRpcResponse,
    ) -> bool {
        self.resolve_server_response_bound(connection_id, None, None, None, response)
            .await
    }

    pub async fn resolve_server_response_bound(
        &self,
        connection_id: ConnectionId,
        method: Option<&str>,
        thread_id: Option<&str>,
        turn_id: Option<&str>,
        response: JsonRpcResponse,
    ) -> bool {
        let pending = {
            let mut requests = self.pending_server_requests.lock().await;
            let matches = requests.get(&response.id).is_some_and(|pending| {
                pending.connection_id == connection_id
                    && method.is_none_or(|method| pending.method == method)
                    && thread_id
                        .is_none_or(|thread_id| pending.thread_id.as_deref() == Some(thread_id))
                    && turn_id.is_none_or(|turn_id| pending.turn_id.as_deref() == Some(turn_id))
            });
            matches.then(|| requests.remove(&response.id)).flatten()
        };
        pending.is_some_and(|pending| pending.callback.send(Ok(response.result)).is_ok())
    }

    pub async fn resolve_server_error(
        &self,
        connection_id: ConnectionId,
        error: JsonRpcError,
    ) -> bool {
        let pending = {
            let mut requests = self.pending_server_requests.lock().await;
            let matches = requests
                .get(&error.id)
                .is_some_and(|pending| pending.connection_id == connection_id);
            matches.then(|| requests.remove(&error.id)).flatten()
        };
        pending.is_some_and(|pending| pending.callback.send(Err(error.error)).is_ok())
    }

    async fn send_message(
        &self,
        connection_id: ConnectionId,
        message: JsonRpcMessage,
    ) -> Result<(), AppServerError> {
        if !self.is_connection_registered(connection_id).await {
            return Err(AppServerError::internal("client_disconnected"));
        }
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

fn server_request_scope(request: &ServerRequest) -> (Option<String>, Option<String>) {
    match request {
        ServerRequest::ApprovalRequest(params) => {
            (Some(params.thread_id.clone()), Some(params.turn_id.clone()))
        }
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
        ServerNotification::ThreadClosed(_) => "thread/closed",
        ServerNotification::ThreadStatusChanged(_) => "thread/status/changed",
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
