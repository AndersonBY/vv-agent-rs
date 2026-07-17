pub mod approval;
pub mod errors;
pub mod initialize;
pub mod item;
pub mod jsonrpc;
pub mod model;
pub mod schema;
pub mod thread;
pub mod turn;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub use approval::{ApprovalDecision, ApprovalRequestParams, ApprovalResolveParams};
pub use initialize::{
    AppClientCapabilities, AppClientInfo, AppServerCapabilities, InitializeParams,
    InitializeResponse,
};
pub use item::{
    map_run_event_to_notifications, AgentMessageDeltaParams, AppItem, AppItemKind, AppItemStatus,
    ItemCompletedParams, ItemStartedParams, ToolCallDeltaParams,
};
pub use model::{AppModelInfo, ModelListParams, ModelListResponse};
pub use schema::{
    generate_app_server_json_schema_bundle, generate_app_server_typescript_bundle,
    AppServerSchemaError, SchemaBundle, SchemaExportResponse,
};
pub use thread::{
    AppThread, ThreadArchiveParams, ThreadArchiveResponse, ThreadArchivedParams,
    ThreadClosedParams, ThreadListParams, ThreadListResponse, ThreadReadParams, ThreadReadResponse,
    ThreadResumeParams, ThreadResumeResponse, ThreadStartParams, ThreadStartResponse,
    ThreadStartedParams, ThreadStatus, ThreadStatusChangedParams, ThreadUnsubscribeParams,
    ThreadUnsubscribeResponse,
};
pub use turn::{
    AppCacheUsage, AppTokenUsage, AppTurn, CheckpointSummary, CheckpointSummaryStatus,
    InterruptionIdempotencySupport, InterruptionOperationKind, InterruptionSummary,
    TurnCompletedParams, TurnControlResponse, TurnFollowUpParams, TurnFollowUpResponse,
    TurnInterruptParams, TurnInterruptResponse, TurnResumeParams, TurnResumeResponse,
    TurnStartParams, TurnStartResponse, TurnStartedParams, TurnStatus, TurnSteerParams,
    TurnSteerResponse, UserInput,
};

pub use errors::{AppServerError, AppServerErrorCode};
pub use jsonrpc::{
    JsonRpcError, JsonRpcErrorBody, JsonRpcMessage, JsonRpcNotification, JsonRpcRequest,
    JsonRpcResponse, RequestId,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
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
    #[serde(rename = "thread/unsubscribe")]
    ThreadUnsubscribe(ThreadUnsubscribeParams),
    #[serde(rename = "turn/start")]
    TurnStart(TurnStartParams),
    #[serde(rename = "turn/resume")]
    TurnResume(TurnResumeParams),
    #[serde(rename = "turn/interrupt")]
    TurnInterrupt(TurnInterruptParams),
    #[serde(rename = "turn/steer")]
    TurnSteer(TurnSteerParams),
    #[serde(rename = "turn/followUp")]
    TurnFollowUp(TurnFollowUpParams),
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
            "thread/unsubscribe",
            "turn/start",
            "turn/resume",
            "turn/interrupt",
            "turn/steer",
            "turn/followUp",
            "approval/resolve",
            "model/list",
            "schema/export",
            "initialized",
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[allow(clippy::large_enum_variant)] // Preserve the stable unboxed protocol enum API.
#[serde(tag = "method", content = "params")]
pub enum ServerNotification {
    #[serde(rename = "thread/started")]
    ThreadStarted(ThreadStartedParams),
    #[serde(rename = "thread/archived")]
    ThreadArchived(ThreadArchivedParams),
    #[serde(rename = "thread/closed")]
    ThreadClosed(ThreadClosedParams),
    #[serde(rename = "thread/status/changed")]
    ThreadStatusChanged(ThreadStatusChangedParams),
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
            "thread/closed",
            "thread/status/changed",
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct WarningParams {
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}
