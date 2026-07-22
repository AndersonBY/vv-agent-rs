use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::events::{ApprovalAction, RunEvent, RunEventPayload, ToolStatus};

use super::approval::{ApprovalDecision, ApprovalRequestParams, ApprovalResolveParams};
use super::ServerNotification;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ItemStartedParams {
    #[serde(flatten)]
    pub item: AppItem,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentMessageDeltaParams {
    #[serde(flatten)]
    pub item: AppItem,
    pub delta: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallDeltaParams {
    #[serde(flatten)]
    pub item: AppItem,
    pub delta: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ItemCompletedParams {
    #[serde(flatten)]
    pub item: AppItem,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct AppItem {
    pub item_id: String,
    pub thread_id: String,
    pub turn_id: String,
    #[serde(rename = "type")]
    pub kind: AppItemKind,
    pub status: AppItemStatus,
    #[serde(default)]
    pub payload: Value,
    pub created_at: f64,
    pub updated_at: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum AppItemKind {
    UserMessage,
    AgentMessage,
    ToolCall,
    Approval,
    Error,
    MemoryCompact,
    SubRun,
    Handoff,
    RunStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum AppItemStatus {
    Queued,
    Started,
    InProgress,
    Completed,
    Failed,
    Interrupted,
}

pub fn map_run_event_to_notifications(
    thread_id: &str,
    turn_id: &str,
    event: &RunEvent,
) -> Vec<ServerNotification> {
    match event.payload() {
        RunEventPayload::RunStarted { input } => {
            let item = item(
                thread_id,
                turn_id,
                event,
                AppItemKind::UserMessage,
                AppItemStatus::Completed,
                serde_json::json!({ "text": input }),
            );
            vec![ServerNotification::ItemCompleted(ItemCompletedParams {
                item,
            })]
        }
        RunEventPayload::AssistantDelta { delta, .. } => {
            let item = item(
                thread_id,
                turn_id,
                event,
                AppItemKind::AgentMessage,
                AppItemStatus::InProgress,
                serde_json::json!({ "delta": delta }),
            );
            vec![ServerNotification::AgentMessageDelta(
                AgentMessageDeltaParams {
                    item,
                    delta: delta.clone(),
                },
            )]
        }
        RunEventPayload::ModelToolCallProgress {
            tool_call_id,
            tool_call_index,
            tool_name,
            arguments_chars,
            ..
        } => {
            let item = item(
                thread_id,
                turn_id,
                event,
                AppItemKind::ToolCall,
                AppItemStatus::InProgress,
                serde_json::json!({
                    "toolCallId": tool_call_id,
                    "toolName": tool_name,
                }),
            );
            vec![ServerNotification::ToolCallDelta(ToolCallDeltaParams {
                item,
                delta: serde_json::json!({
                    "toolCallId": tool_call_id,
                    "toolCallIndex": tool_call_index,
                    "toolName": tool_name,
                    "argumentsChars": arguments_chars,
                }),
            })]
        }
        RunEventPayload::ToolCallPlanned { .. } => Vec::new(),
        RunEventPayload::ToolCallStarted {
            tool_call_id,
            tool_name,
            arguments,
        } => {
            let mut payload = serde_json::json!({
                "toolCallId": tool_call_id,
                "toolName": tool_name,
            });
            if let Some(tool_metadata) = event.tool_metadata() {
                payload["toolMetadata"] = tool_metadata_payload(&tool_metadata);
            }
            let item = item(
                thread_id,
                turn_id,
                event,
                AppItemKind::ToolCall,
                AppItemStatus::Started,
                payload,
            );
            vec![
                ServerNotification::ItemStarted(ItemStartedParams { item: item.clone() }),
                ServerNotification::ToolCallDelta(ToolCallDeltaParams {
                    item,
                    delta: arguments.clone(),
                }),
            ]
        }
        RunEventPayload::ToolCallCompleted {
            tool_call_id,
            tool_name,
            status,
            ..
        } => {
            let mut payload = serde_json::json!({
                "toolCallId": tool_call_id,
                "toolName": tool_name,
                "status": status,
            });
            payload["directive"] = event
                .tool_directive()
                .map(|value| serde_json::to_value(value).expect("directive serializes"))
                .expect("tool completion directive is present");
            payload["errorCode"] = event
                .tool_error_code()
                .map_or(Value::Null, |value| Value::String(value.to_string()));
            payload["executionStarted"] = event
                .tool_execution_started()
                .map(Value::Bool)
                .expect("tool completion execution_started is present");
            payload["durationMs"] = event.tool_duration_ms().map_or(Value::Null, Value::from);
            if let Some(tool_metadata) = event.tool_metadata() {
                payload["toolMetadata"] = tool_metadata_payload(&tool_metadata);
            }
            let item = item(
                thread_id,
                turn_id,
                event,
                AppItemKind::ToolCall,
                tool_status_to_item_status(*status),
                payload,
            );
            vec![ServerNotification::ItemCompleted(ItemCompletedParams {
                item,
            })]
        }
        RunEventPayload::ApprovalRequested {
            request_id,
            tool_call_id,
            tool_name,
            message,
        } => {
            let item = item(
                thread_id,
                turn_id,
                event,
                AppItemKind::Approval,
                AppItemStatus::Started,
                serde_json::json!({
                    "requestId": request_id,
                    "toolCallId": tool_call_id,
                    "toolName": tool_name,
                    "message": message,
                    "arguments": event
                        .metadata()
                        .get("arguments")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({})),
                }),
            );
            vec![
                ServerNotification::ItemStarted(ItemStartedParams { item }),
                ServerNotification::ApprovalRequested(ApprovalRequestParams {
                    request_id: request_id.clone(),
                    thread_id: thread_id.to_string(),
                    turn_id: turn_id.to_string(),
                    tool_call_id: tool_call_id.clone(),
                    tool_name: tool_name.clone(),
                    preview: message.clone(),
                    arguments: event
                        .metadata()
                        .get("arguments")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({})),
                }),
            ]
        }
        RunEventPayload::ApprovalResolved {
            request_id,
            tool_call_id,
            tool_name,
            action,
        } => {
            let decision = match action {
                ApprovalAction::Allow => ApprovalDecision::Allow,
                ApprovalAction::AllowSession => ApprovalDecision::AllowSession,
                ApprovalAction::Deny => ApprovalDecision::Deny,
                ApprovalAction::Timeout => ApprovalDecision::Timeout,
            };
            let reason = event
                .metadata()
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let decision_metadata = event
                .metadata()
                .get("decision_metadata")
                .and_then(Value::as_object)
                .map(|metadata| {
                    metadata
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect()
                })
                .unwrap_or_default();
            let item = item(
                thread_id,
                turn_id,
                event,
                AppItemKind::Approval,
                AppItemStatus::Completed,
                serde_json::json!({
                    "requestId": request_id,
                    "toolCallId": tool_call_id,
                    "toolName": tool_name,
                    "action": action.as_str(),
                    "approved": action.is_approved(),
                    "reason": reason,
                    "decisionMetadata": decision_metadata,
                }),
            );
            vec![
                ServerNotification::ItemCompleted(ItemCompletedParams { item }),
                ServerNotification::ApprovalResolved(ApprovalResolveParams {
                    thread_id: thread_id.to_string(),
                    turn_id: turn_id.to_string(),
                    request_id: request_id.clone(),
                    decision,
                    reason,
                    metadata: decision_metadata,
                }),
            ]
        }
        RunEventPayload::RunCompleted { .. } => {
            event.final_output().map_or_else(Vec::new, |output| {
                let item = item(
                    thread_id,
                    turn_id,
                    event,
                    AppItemKind::AgentMessage,
                    AppItemStatus::Completed,
                    serde_json::json!({ "text": output }),
                );
                vec![ServerNotification::ItemCompleted(ItemCompletedParams {
                    item,
                })]
            })
        }
        RunEventPayload::RunFailed { error } => {
            let item = item(
                thread_id,
                turn_id,
                event,
                AppItemKind::Error,
                AppItemStatus::Completed,
                serde_json::json!({ "message": error }),
            );
            vec![
                ServerNotification::ItemCompleted(ItemCompletedParams { item }),
                ServerNotification::ErrorWarning(super::WarningParams {
                    message: error.clone(),
                    code: Some("run_failed".to_string()),
                }),
            ]
        }
        _ => Vec::new(),
    }
}

fn item(
    thread_id: &str,
    turn_id: &str,
    event: &RunEvent,
    kind: AppItemKind,
    status: AppItemStatus,
    payload: Value,
) -> AppItem {
    AppItem {
        item_id: format!("item_{}", event.event_id().as_str()),
        thread_id: thread_id.to_string(),
        turn_id: turn_id.to_string(),
        kind,
        status,
        payload,
        created_at: event_timestamp(event),
        updated_at: event_timestamp(event),
    }
}

fn event_timestamp(event: &RunEvent) -> f64 {
    event.created_at()
}

fn tool_status_to_item_status(status: ToolStatus) -> AppItemStatus {
    match status {
        ToolStatus::Started => AppItemStatus::Started,
        ToolStatus::Success => AppItemStatus::Completed,
        ToolStatus::Error => AppItemStatus::Failed,
        ToolStatus::WaitResponse | ToolStatus::Running | ToolStatus::PendingCompress => {
            AppItemStatus::InProgress
        }
    }
}

fn tool_metadata_payload(metadata: &crate::tools::ToolMetadata) -> Value {
    serde_json::json!({
        "sideEffect": metadata.side_effect,
        "idempotency": metadata.idempotency,
        "terminal": metadata.terminal,
        "capabilityTags": metadata.capability_tags,
        "costDimensions": metadata.cost_dimensions,
    })
}
