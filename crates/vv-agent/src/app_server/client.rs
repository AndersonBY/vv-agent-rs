use std::collections::VecDeque;
use std::fmt;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::app_server::outgoing::OutgoingEnvelope;
use crate::app_server::processor::MessageProcessor;
use crate::app_server::protocol::{
    AppClientCapabilities, AppClientInfo, ApprovalDecision, ApprovalResolveParams,
    InitializeParams, InitializeResponse, JsonRpcMessage, JsonRpcNotification, JsonRpcRequest,
    JsonRpcResponse, RequestId, ThreadReadParams, ThreadReadResponse, ThreadStartParams,
    ThreadStartResponse, TurnStartParams, TurnStartResponse,
};
use crate::app_server::transport::ConnectionId;

pub struct AppServerClient {
    processor: MessageProcessor,
    outgoing: mpsc::Receiver<OutgoingEnvelope>,
    connection_id: ConnectionId,
    next_request_id: i64,
    backlog: VecDeque<JsonRpcMessage>,
}

impl AppServerClient {
    pub fn new_for_processor(
        processor: MessageProcessor,
        outgoing: mpsc::Receiver<OutgoingEnvelope>,
        connection_id: ConnectionId,
    ) -> Self {
        Self {
            processor,
            outgoing,
            connection_id,
            next_request_id: 1,
            backlog: VecDeque::new(),
        }
    }

    pub async fn initialize(
        &mut self,
        info: AppClientInfo,
    ) -> Result<InitializeResponse, AppServerClientError> {
        let response = self
            .send_request(
                "initialize",
                serde_json::to_value(InitializeParams {
                    client_info: info,
                    capabilities: AppClientCapabilities::default(),
                })?,
            )
            .await?;
        self.processor
            .process_message(
                self.connection_id,
                JsonRpcMessage::Notification(JsonRpcNotification {
                    method: "initialized".to_string(),
                    params: None,
                }),
            )
            .await;
        Ok(response)
    }

    pub async fn start_thread(
        &mut self,
        params: ThreadStartParams,
    ) -> Result<ThreadStartResponse, AppServerClientError> {
        self.send_request("thread/start", serde_json::to_value(params)?)
            .await
    }

    pub async fn start_turn(
        &mut self,
        params: TurnStartParams,
    ) -> Result<TurnStartResponse, AppServerClientError> {
        self.send_request("turn/start", serde_json::to_value(params)?)
            .await
    }

    pub async fn resolve_approval(
        &mut self,
        params: ApprovalResolveParams,
    ) -> Result<(), AppServerClientError> {
        let decision = match params.decision {
            ApprovalDecision::Allow => "allow",
            ApprovalDecision::Deny => "deny",
        };
        self.processor
            .process_message(
                self.connection_id,
                JsonRpcMessage::Response(JsonRpcResponse {
                    id: RequestId::String(params.request_id),
                    result: json!({ "decision": decision }),
                }),
            )
            .await;
        Ok(())
    }

    pub async fn read_thread(
        &mut self,
        params: ThreadReadParams,
    ) -> Result<ThreadReadResponse, AppServerClientError> {
        self.send_request("thread/read", serde_json::to_value(params)?)
            .await
    }

    pub async fn next_message(&mut self) -> Option<JsonRpcMessage> {
        if let Some(message) = self.backlog.pop_front() {
            return Some(message);
        }
        self.next_outgoing_message().await.ok()
    }

    async fn send_request<T: DeserializeOwned>(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<T, AppServerClientError> {
        let id = RequestId::Integer(self.next_request_id);
        self.next_request_id += 1;
        self.processor
            .process_message(
                self.connection_id,
                JsonRpcMessage::Request(JsonRpcRequest {
                    id: id.clone(),
                    method: method.to_string(),
                    params: Some(params),
                }),
            )
            .await;
        self.wait_response(id).await
    }

    async fn wait_response<T: DeserializeOwned>(
        &mut self,
        id: RequestId,
    ) -> Result<T, AppServerClientError> {
        loop {
            match self.next_outgoing_message().await? {
                JsonRpcMessage::Response(response) if response.id == id => {
                    return Ok(serde_json::from_value(response.result)?);
                }
                JsonRpcMessage::Error(error) if error.id == id => {
                    return Err(AppServerClientError::server(error.error.message));
                }
                message => self.backlog.push_back(message),
            }
        }
    }

    async fn next_outgoing_message(&mut self) -> Result<JsonRpcMessage, AppServerClientError> {
        loop {
            let envelope = tokio::time::timeout(Duration::from_secs(3), self.outgoing.recv())
                .await
                .map_err(|_| AppServerClientError::timeout("timed out waiting for message"))?
                .ok_or_else(|| AppServerClientError::transport("outgoing channel closed"))?;
            if envelope.connection_id == self.connection_id {
                return Ok(envelope.message);
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppServerClientError {
    message: String,
}

impl AppServerClientError {
    fn server(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    fn timeout(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    fn transport(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for AppServerClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AppServerClientError {}

impl From<serde_json::Error> for AppServerClientError {
    fn from(error: serde_json::Error) -> Self {
        Self {
            message: error.to_string(),
        }
    }
}
