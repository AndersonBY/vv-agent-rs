use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::app_server::outgoing::{OutgoingEnvelope, OutgoingMessageSender};
use crate::app_server::protocol::{
    AppClientInfo, AppServerCapabilities, AppServerError, AppServerErrorCode, InitializeParams,
    InitializeResponse, JsonRpcMessage, JsonRpcNotification, JsonRpcRequest,
};
use crate::app_server::transport::ConnectionId;

pub struct MessageProcessor {
    outgoing: OutgoingMessageSender,
    connections: HashMap<ConnectionId, ConnectionSessionState>,
}

#[derive(Debug, Clone, Default)]
pub struct ConnectionSessionState {
    initialized: bool,
    ready_for_notifications: bool,
    client_info: Option<AppClientInfo>,
    experimental_api: bool,
    opt_out_notification_methods: HashSet<String>,
}

impl ConnectionSessionState {
    pub fn initialized(&self) -> bool {
        self.initialized
    }

    pub fn ready_for_notifications(&self) -> bool {
        self.ready_for_notifications
    }

    pub fn client_info(&self) -> Option<&AppClientInfo> {
        self.client_info.as_ref()
    }

    pub fn experimental_api(&self) -> bool {
        self.experimental_api
    }

    pub fn opt_out_notification_methods(&self) -> &HashSet<String> {
        &self.opt_out_notification_methods
    }
}

impl MessageProcessor {
    pub fn new_for_tests(
        outgoing_capacity: usize,
    ) -> (Self, tokio::sync::mpsc::Receiver<OutgoingEnvelope>) {
        let (outgoing, rx) = OutgoingMessageSender::channel(outgoing_capacity);
        (
            Self {
                outgoing,
                connections: HashMap::new(),
            },
            rx,
        )
    }

    pub fn outgoing(&self) -> &OutgoingMessageSender {
        &self.outgoing
    }

    pub fn connection_state(&self, connection_id: ConnectionId) -> Option<&ConnectionSessionState> {
        self.connections.get(&connection_id)
    }

    pub async fn process_message(&mut self, connection_id: ConnectionId, message: JsonRpcMessage) {
        self.outgoing.register_connection(connection_id).await;
        match message {
            JsonRpcMessage::Request(request) => {
                self.process_request(connection_id, request).await;
            }
            JsonRpcMessage::Notification(notification) => {
                self.process_notification(connection_id, notification).await;
            }
            JsonRpcMessage::Response(response) => {
                let _ = self.outgoing.resolve_server_response(response).await;
            }
            JsonRpcMessage::Error(error) => {
                let _ = self.outgoing.resolve_server_error(error).await;
            }
        }
    }

    async fn process_request(&mut self, connection_id: ConnectionId, request: JsonRpcRequest) {
        if request.method == "initialize" {
            self.process_initialize(connection_id, request).await;
            return;
        }

        if !self
            .connections
            .get(&connection_id)
            .is_some_and(ConnectionSessionState::initialized)
        {
            let _ = self
                .outgoing
                .send_error(connection_id, request.id, AppServerError::not_initialized())
                .await;
            return;
        }

        let _ = self
            .outgoing
            .send_error(
                connection_id,
                request.id,
                AppServerError::new(
                    AppServerErrorCode::MethodNotFound,
                    format!("Method not found: {}", request.method),
                )
                .with_data(serde_json::json!({ "method": request.method })),
            )
            .await;
    }

    async fn process_initialize(&mut self, connection_id: ConnectionId, request: JsonRpcRequest) {
        let state = self.connections.entry(connection_id).or_default();
        if state.initialized {
            let _ = self
                .outgoing
                .send_error(
                    connection_id,
                    request.id,
                    AppServerError::already_initialized(),
                )
                .await;
            return;
        }

        let params = match parse_params::<InitializeParams>(request.params) {
            Ok(params) => params,
            Err(error) => {
                let _ = self
                    .outgoing
                    .send_error(connection_id, request.id, error)
                    .await;
                return;
            }
        };
        state.initialized = true;
        state.client_info = Some(params.client_info);
        state.experimental_api = params.capabilities.experimental_api;
        state.opt_out_notification_methods = params
            .capabilities
            .opt_out_notification_methods
            .into_iter()
            .collect();
        self.outgoing
            .configure_connection(connection_id, state.opt_out_notification_methods.clone())
            .await;

        let result = serde_json::to_value(InitializeResponse::new(
            "vv-agent-rs",
            env!("CARGO_PKG_VERSION"),
            AppServerCapabilities::mvp(),
        ))
        .expect("initialize response serializes");
        let _ = self
            .outgoing
            .send_response(connection_id, request.id, result)
            .await;
    }

    async fn process_notification(
        &mut self,
        connection_id: ConnectionId,
        notification: JsonRpcNotification,
    ) {
        if notification.method != "initialized" {
            return;
        }
        let Some(state) = self.connections.get_mut(&connection_id) else {
            return;
        };
        if !state.initialized {
            return;
        }
        state.ready_for_notifications = true;
        self.outgoing
            .mark_ready_for_notifications(connection_id)
            .await;
    }
}

fn parse_params<T: serde::de::DeserializeOwned>(
    params: Option<Value>,
) -> Result<T, AppServerError> {
    let params = params.ok_or_else(|| AppServerError::invalid_params("Missing params"))?;
    serde_json::from_value(params)
        .map_err(|error| AppServerError::invalid_params(error.to_string()))
}
