use serde_json::Value;

use crate::events::{RunEvent, ToolStatus};
use crate::result::RunResult;

pub struct RunEventStream {
    events: std::vec::IntoIter<Result<RunEvent, String>>,
    result: Option<RunResult>,
}

impl RunEventStream {
    pub(super) fn from_events(result: RunResult, events: Vec<RunEvent>) -> Self {
        let events = events.into_iter().map(Ok).collect::<Vec<_>>().into_iter();
        Self {
            events,
            result: Some(result),
        }
    }

    pub async fn next(&mut self) -> Option<Result<RunEvent, String>> {
        self.events.next()
    }

    pub async fn into_result(mut self) -> Result<RunResult, String> {
        self.result
            .take()
            .ok_or_else(|| "stream result already taken".to_string())
    }
}

pub(super) fn map_runtime_event(
    event: &str,
    payload: &std::collections::BTreeMap<String, Value>,
) -> Option<RunEvent> {
    let run_id = payload
        .get("task_id")
        .and_then(Value::as_str)
        .or_else(|| payload.get("run_id").and_then(Value::as_str))
        .unwrap_or("run")
        .to_string();
    match event {
        "run_started" => Some(RunEvent::RunStarted {
            run_id,
            agent_name: payload
                .get("agent_name")
                .and_then(Value::as_str)
                .unwrap_or("agent")
                .to_string(),
        }),
        "cycle_started" => Some(RunEvent::AgentStarted {
            run_id,
            agent_name: payload
                .get("agent_name")
                .and_then(Value::as_str)
                .unwrap_or("agent")
                .to_string(),
            cycle_index: payload
                .get("cycle")
                .and_then(Value::as_u64)
                .unwrap_or_default() as u32,
        }),
        "cycle_llm_response" => {
            let cycle_index = payload
                .get("cycle")
                .and_then(Value::as_u64)
                .unwrap_or_default() as u32;
            payload
                .get("assistant_message")
                .and_then(Value::as_str)
                .filter(|delta| !delta.is_empty())
                .map(|delta| RunEvent::AssistantDelta {
                    run_id,
                    delta: delta.to_string(),
                    cycle_index,
                })
        }
        "tool_result" => {
            let metadata = payload.get("metadata").and_then(Value::as_object);
            if let Some(interruption_id) = metadata
                .and_then(|metadata| metadata.get("approval_interruption_id"))
                .and_then(Value::as_str)
            {
                return Some(RunEvent::ToolApprovalRequested {
                    run_id,
                    interruption_id: interruption_id.to_string(),
                    tool_name: payload
                        .get("tool_name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                });
            }
            let status = match payload
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default()
            {
                "error" | "ERROR" => ToolStatus::Error,
                "wait_response" | "WAIT_RESPONSE" => ToolStatus::WaitResponse,
                _ => ToolStatus::Success,
            };
            Some(RunEvent::ToolFinished {
                run_id,
                tool_call_id: payload
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                tool_name: payload
                    .get("tool_name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                status,
            })
        }
        "run_completed" => Some(RunEvent::RunCompleted {
            run_id,
            status: crate::types::AgentStatus::Completed,
        }),
        "run_wait_user" => Some(RunEvent::RunCompleted {
            run_id,
            status: crate::types::AgentStatus::WaitUser,
        }),
        "run_max_cycles" => Some(RunEvent::RunCompleted {
            run_id,
            status: crate::types::AgentStatus::MaxCycles,
        }),
        "cycle_failed" => Some(RunEvent::RunFailed {
            run_id,
            error: crate::events::AgentErrorPayload::new(
                payload
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("cycle failed"),
            ),
        }),
        _ => None,
    }
}
