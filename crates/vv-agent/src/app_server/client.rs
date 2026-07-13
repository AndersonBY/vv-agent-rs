use std::collections::VecDeque;
use std::fmt;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::{json, Map, Value};
use tokio::sync::mpsc;

use crate::app_server::outgoing::OutgoingEnvelope;
use crate::app_server::processor::MessageProcessor;
use crate::app_server::protocol::{
    AppClientCapabilities, AppClientInfo, ApprovalResolveParams, InitializeParams,
    InitializeResponse, JsonRpcErrorBody, JsonRpcMessage, JsonRpcNotification, JsonRpcRequest,
    JsonRpcResponse, ModelListParams, ModelListResponse, RequestId, SchemaExportResponse,
    ThreadArchiveParams, ThreadArchiveResponse, ThreadListParams, ThreadListResponse,
    ThreadReadParams, ThreadReadResponse, ThreadResumeParams, ThreadResumeResponse,
    ThreadStartParams, ThreadStartResponse, ThreadUnsubscribeParams, ThreadUnsubscribeResponse,
    TurnFollowUpParams, TurnFollowUpResponse, TurnInterruptParams, TurnInterruptResponse,
    TurnStartParams, TurnStartResponse, TurnSteerParams, TurnSteerResponse,
};
use crate::app_server::transport::ConnectionId;

const RESPONSE_TIMEOUT: Duration = Duration::from_secs(3);

pub struct AppServerClient {
    processor: MessageProcessor,
    outgoing: mpsc::Receiver<OutgoingEnvelope>,
    connection_id: ConnectionId,
    next_request_id: i64,
    backlog: VecDeque<JsonRpcMessage>,
    closed: bool,
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
            closed: false,
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
        self.send_notification("initialized", None).await?;
        Ok(response)
    }

    pub async fn start_thread(
        &mut self,
        params: ThreadStartParams,
    ) -> Result<ThreadStartResponse, AppServerClientError> {
        self.send_request("thread/start", serde_json::to_value(params)?)
            .await
    }

    pub async fn resume_thread(
        &mut self,
        params: ThreadResumeParams,
    ) -> Result<ThreadResumeResponse, AppServerClientError> {
        self.send_request("thread/resume", serde_json::to_value(params)?)
            .await
    }

    pub async fn read_thread(
        &mut self,
        params: ThreadReadParams,
    ) -> Result<ThreadReadResponse, AppServerClientError> {
        self.send_request("thread/read", serde_json::to_value(params)?)
            .await
    }

    pub async fn list_threads(
        &mut self,
        params: ThreadListParams,
    ) -> Result<ThreadListResponse, AppServerClientError> {
        self.send_request("thread/list", serde_json::to_value(params)?)
            .await
    }

    pub async fn archive_thread(
        &mut self,
        params: ThreadArchiveParams,
    ) -> Result<ThreadArchiveResponse, AppServerClientError> {
        self.send_request("thread/archive", serde_json::to_value(params)?)
            .await
    }

    pub async fn unsubscribe_thread(
        &mut self,
        params: ThreadUnsubscribeParams,
    ) -> Result<ThreadUnsubscribeResponse, AppServerClientError> {
        self.send_request("thread/unsubscribe", serde_json::to_value(params)?)
            .await
    }

    pub async fn start_turn(
        &mut self,
        params: TurnStartParams,
    ) -> Result<TurnStartResponse, AppServerClientError> {
        self.send_request("turn/start", serde_json::to_value(params)?)
            .await
    }

    pub async fn interrupt_turn(
        &mut self,
        params: TurnInterruptParams,
    ) -> Result<TurnInterruptResponse, AppServerClientError> {
        self.send_request("turn/interrupt", serde_json::to_value(params)?)
            .await
    }

    pub async fn steer_turn(
        &mut self,
        params: TurnSteerParams,
    ) -> Result<TurnSteerResponse, AppServerClientError> {
        self.send_request("turn/steer", serde_json::to_value(params)?)
            .await
    }

    pub async fn follow_up_turn(
        &mut self,
        params: TurnFollowUpParams,
    ) -> Result<TurnFollowUpResponse, AppServerClientError> {
        self.send_request("turn/followUp", serde_json::to_value(params)?)
            .await
    }

    pub async fn resolve_approval(
        &mut self,
        params: ApprovalResolveParams,
    ) -> Result<(), AppServerClientError> {
        self.send_response(
            RequestId::String(params.request_id),
            json!({
                "decision": params.decision.as_wire(),
                "reason": params.reason,
                "metadata": params.metadata,
            }),
        )
        .await
    }

    pub async fn resolve_approval_request(
        &mut self,
        params: ApprovalResolveParams,
    ) -> Result<(), AppServerClientError> {
        let _: Map<String, Value> = self
            .send_request("approval/resolve", serde_json::to_value(params)?)
            .await?;
        Ok(())
    }

    pub async fn send_response(
        &mut self,
        request_id: RequestId,
        result: Value,
    ) -> Result<(), AppServerClientError> {
        self.ensure_open()?;
        self.processor
            .process_message(
                self.connection_id,
                JsonRpcMessage::Response(JsonRpcResponse {
                    id: request_id,
                    result,
                }),
            )
            .await;
        Ok(())
    }

    pub async fn list_models(
        &mut self,
        params: ModelListParams,
    ) -> Result<ModelListResponse, AppServerClientError> {
        self.send_request("model/list", serde_json::to_value(params)?)
            .await
    }

    pub async fn export_schema(&mut self) -> Result<SchemaExportResponse, AppServerClientError> {
        self.send_request("schema/export", json!({})).await
    }

    pub async fn next_message(&mut self) -> Option<JsonRpcMessage> {
        self.try_next_message().await.ok()
    }

    pub async fn try_next_message(&mut self) -> Result<JsonRpcMessage, AppServerClientError> {
        self.ensure_open()?;
        if let Some(message) = self.backlog.pop_front() {
            return Ok(message);
        }
        self.next_outgoing_message("timed out waiting for App Server message")
            .await
    }

    pub async fn close(&mut self) -> bool {
        if self.closed {
            return false;
        }
        self.closed = true;
        self.backlog.clear();
        self.processor
            .disconnect_connection(self.connection_id)
            .await;
        true
    }

    async fn send_request<T: DeserializeOwned>(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<T, AppServerClientError> {
        self.ensure_open()?;
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
        self.wait_response(id, method).await
    }

    async fn send_notification(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> Result<(), AppServerClientError> {
        self.ensure_open()?;
        self.processor
            .process_message(
                self.connection_id,
                JsonRpcMessage::Notification(JsonRpcNotification {
                    method: method.to_string(),
                    params,
                }),
            )
            .await;
        Ok(())
    }

    async fn wait_response<T: DeserializeOwned>(
        &mut self,
        id: RequestId,
        method: &str,
    ) -> Result<T, AppServerClientError> {
        for _ in 0..self.backlog.len() {
            let message = self
                .backlog
                .pop_front()
                .expect("backlog length was captured before scanning");
            match message {
                JsonRpcMessage::Response(response) if response.id == id => {
                    return Self::decode_response(response.result, method);
                }
                JsonRpcMessage::Error(error) if error.id == id => {
                    return Err(AppServerClientError::server(error.error));
                }
                message => self.backlog.push_back(message),
            }
        }

        loop {
            let timeout_message = format!("timed out waiting for App Server {method} response");
            match self.next_outgoing_message(&timeout_message).await? {
                JsonRpcMessage::Response(response) if response.id == id => {
                    return Self::decode_response(response.result, method);
                }
                JsonRpcMessage::Error(error) if error.id == id => {
                    return Err(AppServerClientError::server(error.error));
                }
                message => self.backlog.push_back(message),
            }
        }
    }

    fn decode_response<T: DeserializeOwned>(
        result: Value,
        method: &str,
    ) -> Result<T, AppServerClientError> {
        serde_json::from_value(result).map_err(|error| {
            AppServerClientError::client(format!(
                "failed decoding App Server {method} response: {error}"
            ))
        })
    }

    async fn next_outgoing_message(
        &mut self,
        timeout_message: &str,
    ) -> Result<JsonRpcMessage, AppServerClientError> {
        loop {
            let envelope = tokio::time::timeout(RESPONSE_TIMEOUT, self.outgoing.recv())
                .await
                .map_err(|_| AppServerClientError::timeout(timeout_message))?
                .ok_or_else(|| AppServerClientError::transport("outgoing channel closed"))?;
            if envelope.connection_id == self.connection_id {
                return Ok(envelope.message);
            }
        }
    }

    fn ensure_open(&self) -> Result<(), AppServerClientError> {
        if self.closed {
            Err(AppServerClientError::closed())
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppServerClientError {
    message: String,
    code: Option<i64>,
    data: Option<Value>,
}

impl AppServerClientError {
    fn server(error: JsonRpcErrorBody) -> Self {
        Self {
            message: error.message,
            code: Some(error.code),
            data: error.data,
        }
    }

    fn timeout(message: impl Into<String>) -> Self {
        Self::client(message)
    }

    fn transport(message: impl Into<String>) -> Self {
        Self::client(message)
    }

    fn closed() -> Self {
        Self::client("App Server client is closed")
    }

    fn client(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: None,
            data: None,
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn code(&self) -> Option<i64> {
        self.code
    }

    pub fn data(&self) -> Option<&Value> {
        self.data.as_ref()
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
        Self::client(error.to_string())
    }
}
