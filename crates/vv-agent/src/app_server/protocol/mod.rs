pub mod approval;
pub mod errors;
pub mod initialize;
pub mod item;
pub mod jsonrpc;
pub mod model;
pub mod thread;
pub mod turn;

use serde::{Deserialize, Serialize};

pub use approval::{ApprovalDecision, ApprovalRequestParams, ApprovalResolveParams};
pub use initialize::{
    AppClientCapabilities, AppClientInfo, AppServerCapabilities, AppServerInfo, InitializeParams,
    InitializeResponse,
};
pub use item::{
    AgentMessageDeltaParams, AppItem, AppItemKind, AppItemStatus, ItemCompletedParams,
    ItemStartedParams, ToolCallDeltaParams,
};
pub use model::{AppModelInfo, ModelListParams, ModelListResponse};
pub use thread::{
    AppThread, ThreadArchiveParams, ThreadArchiveResponse, ThreadArchivedParams, ThreadListParams,
    ThreadListResponse, ThreadReadParams, ThreadReadResponse, ThreadResumeParams,
    ThreadResumeResponse, ThreadStartParams, ThreadStartResponse, ThreadStartedParams,
    ThreadStatus,
};
pub use turn::{
    AppTokenUsage, AppTurn, TurnCompletedParams, TurnInterruptParams, TurnInterruptResponse,
    TurnStartParams, TurnStartResponse, TurnStartedParams, TurnStatus, TurnSteerParams,
    TurnSteerResponse, UserInput,
};

pub use errors::AppServerErrorCode;
pub use jsonrpc::{
    JsonRpcError, JsonRpcErrorBody, JsonRpcMessage, JsonRpcNotification, JsonRpcRequest,
    JsonRpcResponse, RequestId,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum ClientRequest {
    #[serde(rename = "initialize")]
    Initialize(InitializeParams),
    #[serde(rename = "thread/start")]
    ThreadStart(ThreadStartParams),
    #[serde(rename = "thread/resume")]
    ThreadResume(ThreadResumeParams),
    #[serde(rename = "thread/read")]
    ThreadRead(ThreadReadParams),
    #[serde(rename = "thread/list")]
    ThreadList(ThreadListParams),
    #[serde(rename = "thread/archive")]
    ThreadArchive(ThreadArchiveParams),
    #[serde(rename = "turn/start")]
    TurnStart(TurnStartParams),
    #[serde(rename = "turn/interrupt")]
    TurnInterrupt(TurnInterruptParams),
    #[serde(rename = "turn/steer")]
    TurnSteer(TurnSteerParams),
    #[serde(rename = "approval/resolve")]
    ApprovalResolve(ApprovalResolveParams),
    #[serde(rename = "model/list")]
    ModelList(ModelListParams),
    #[serde(rename = "schema/export")]
    SchemaExport,
    #[serde(rename = "initialized")]
    Initialized,
}

impl ClientRequest {
    pub fn stable_method_names() -> Vec<&'static str> {
        vec![
            "initialize",
            "thread/start",
            "thread/resume",
            "thread/read",
            "thread/list",
            "thread/archive",
            "turn/start",
            "turn/interrupt",
            "turn/steer",
            "approval/resolve",
            "model/list",
            "schema/export",
            "initialized",
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum ServerNotification {
    #[serde(rename = "thread/started")]
    ThreadStarted(ThreadStartedParams),
    #[serde(rename = "thread/archived")]
    ThreadArchived(ThreadArchivedParams),
    #[serde(rename = "turn/started")]
    TurnStarted(TurnStartedParams),
    #[serde(rename = "turn/completed")]
    TurnCompleted(TurnCompletedParams),
    #[serde(rename = "item/started")]
    ItemStarted(ItemStartedParams),
    #[serde(rename = "item/agentMessage/delta")]
    AgentMessageDelta(AgentMessageDeltaParams),
    #[serde(rename = "item/toolCall/delta")]
    ToolCallDelta(ToolCallDeltaParams),
    #[serde(rename = "item/completed")]
    ItemCompleted(ItemCompletedParams),
    #[serde(rename = "approval/requested")]
    ApprovalRequested(ApprovalRequestParams),
    #[serde(rename = "approval/resolved")]
    ApprovalResolved(ApprovalResolveParams),
    #[serde(rename = "error/warning")]
    ErrorWarning(WarningParams),
}

impl ServerNotification {
    pub fn stable_method_names() -> Vec<&'static str> {
        vec![
            "thread/started",
            "thread/archived",
            "turn/started",
            "turn/completed",
            "item/started",
            "item/agentMessage/delta",
            "item/toolCall/delta",
            "item/completed",
            "approval/requested",
            "approval/resolved",
            "error/warning",
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum ServerRequest {
    #[serde(rename = "approval/request")]
    ApprovalRequest(ApprovalRequestParams),
}

impl ServerRequest {
    pub fn stable_method_names() -> Vec<&'static str> {
        vec!["approval/request"]
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WarningParams {
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}
