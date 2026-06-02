use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::events::{RunEvent, RunEventPayload, ToolStatus};
use crate::types::AgentStatus;

use super::approval::{ApprovalDecision, ApprovalRequestParams};
use super::turn::{AppTurn, TurnCompletedParams, TurnStatus};
use super::ServerNotification;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ItemStartedParams {
    pub thread_id: String,
    pub turn_id: String,
    pub item: AppItem,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentMessageDeltaParams {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallDeltaParams {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ItemCompletedParams {
    pub thread_id: String,
    pub turn_id: String,
    pub item: AppItem,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct AppItem {
    pub id: String,
    pub run_event_id: String,
    #[serde(rename = "type")]
    pub kind: AppItemKind,
    pub status: AppItemStatus,
    pub created_at_ms: u128,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_ms: Option<u128>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum AppItemKind {
    UserMessage,
    AgentMessage,
    ToolCall,
    ApprovalRequest,
    ApprovalResolved,
    MemoryCompact,
    SubRun,
    Handoff,
    RunStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum AppItemStatus {
    Queued,
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
        RunEventPayload::AssistantDelta { delta } => {
            vec![ServerNotification::AgentMessageDelta(
                AgentMessageDeltaParams {
                    thread_id: thread_id.to_string(),
                    turn_id: turn_id.to_string(),
                    item_id: event.event_id().as_str().to_string(),
                    delta: delta.clone(),
                },
            )]
        }
        RunEventPayload::ToolCallStarted {
            tool_call_id,
            tool_name,
            arguments,
        } => vec![ServerNotification::ItemStarted(ItemStartedParams {
            thread_id: thread_id.to_string(),
            turn_id: turn_id.to_string(),
            item: AppItem {
                id: event.event_id().as_str().to_string(),
                run_event_id: event.event_id().as_str().to_string(),
                kind: AppItemKind::ToolCall,
                status: AppItemStatus::InProgress,
                created_at_ms: event.created_at_ms(),
                completed_at_ms: None,
                content: Some(serde_json::json!({
                    "toolCallId": tool_call_id,
                    "toolName": tool_name,
                    "arguments": arguments,
                })),
            },
        })],
        RunEventPayload::ToolCallCompleted {
            tool_call_id,
            tool_name,
            status,
        } => vec![ServerNotification::ItemCompleted(ItemCompletedParams {
            thread_id: thread_id.to_string(),
            turn_id: turn_id.to_string(),
            item: AppItem {
                id: event.event_id().as_str().to_string(),
                run_event_id: event.event_id().as_str().to_string(),
                kind: AppItemKind::ToolCall,
                status: tool_status_to_item_status(*status),
                created_at_ms: event.created_at_ms(),
                completed_at_ms: Some(event.created_at_ms()),
                content: Some(serde_json::json!({
                    "toolCallId": tool_call_id,
                    "toolName": tool_name,
                    "status": status,
                })),
            },
        })],
        RunEventPayload::ApprovalRequested {
            request_id,
            tool_name,
            preview,
            ..
        } => vec![ServerNotification::ApprovalRequested(
            ApprovalRequestParams {
                thread_id: thread_id.to_string(),
                turn_id: turn_id.to_string(),
                request_id: request_id.clone(),
                tool_name: tool_name.clone(),
                preview: preview.clone(),
                choices: vec![ApprovalDecision::Allow, ApprovalDecision::Deny],
            },
        )],
        RunEventPayload::RunCompleted { status } => {
            vec![ServerNotification::TurnCompleted(TurnCompletedParams {
                turn: completed_turn(
                    thread_id,
                    turn_id,
                    event,
                    agent_status_to_turn_status(status),
                ),
            })]
        }
        RunEventPayload::RunFailed { .. } => {
            vec![ServerNotification::TurnCompleted(TurnCompletedParams {
                turn: completed_turn(thread_id, turn_id, event, TurnStatus::Failed),
            })]
        }
        RunEventPayload::RunCancelled { .. } => {
            vec![ServerNotification::TurnCompleted(TurnCompletedParams {
                turn: completed_turn(thread_id, turn_id, event, TurnStatus::Interrupted),
            })]
        }
        _ => Vec::new(),
    }
}

fn completed_turn(thread_id: &str, turn_id: &str, event: &RunEvent, status: TurnStatus) -> AppTurn {
    AppTurn {
        id: turn_id.to_string(),
        thread_id: thread_id.to_string(),
        run_id: event.run_id().to_string(),
        status,
        input: Vec::new(),
        started_at_ms: None,
        completed_at_ms: Some(event.created_at_ms()),
        token_usage: None,
    }
}

fn tool_status_to_item_status(status: ToolStatus) -> AppItemStatus {
    match status {
        ToolStatus::Started => AppItemStatus::InProgress,
        ToolStatus::Success => AppItemStatus::Completed,
        ToolStatus::Error => AppItemStatus::Failed,
        ToolStatus::WaitResponse => AppItemStatus::InProgress,
    }
}

fn agent_status_to_turn_status(status: &AgentStatus) -> TurnStatus {
    match status {
        AgentStatus::Completed => TurnStatus::Completed,
        AgentStatus::Failed => TurnStatus::Failed,
        AgentStatus::Pending => TurnStatus::Queued,
        AgentStatus::Running => TurnStatus::Running,
        AgentStatus::WaitUser => TurnStatus::Running,
        AgentStatus::MaxCycles => TurnStatus::Failed,
    }
}
