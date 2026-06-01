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
    let trace_id = payload
        .get("trace_id")
        .and_then(Value::as_str)
        .unwrap_or(&run_id)
        .to_string();
    let agent_name = payload
        .get("agent_name")
        .and_then(Value::as_str)
        .unwrap_or("agent")
        .to_string();
    match event {
        "run_started" => Some(RunEvent::run_started(
            run_id,
            trace_id,
            agent_name,
            payload
                .get("input")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )),
        "cycle_started" => Some(RunEvent::cycle_started(
            run_id,
            trace_id,
            agent_name,
            payload
                .get("cycle")
                .and_then(Value::as_u64)
                .unwrap_or_default() as u32,
        )),
        "cycle_llm_response" => {
            let cycle_index = payload
                .get("cycle")
                .and_then(Value::as_u64)
                .unwrap_or_default() as u32;
            payload
                .get("assistant_message")
                .and_then(Value::as_str)
                .filter(|delta| !delta.is_empty())
                .map(|delta| {
                    RunEvent::assistant_delta(run_id, trace_id, agent_name, cycle_index, delta)
                })
        }
        "tool_result" => {
            let metadata = payload.get("metadata").and_then(Value::as_object);
            if let Some(interruption_id) = metadata
                .and_then(|metadata| metadata.get("approval_interruption_id"))
                .and_then(Value::as_str)
            {
                return Some(RunEvent::approval_requested(
                    run_id,
                    trace_id,
                    agent_name,
                    interruption_id,
                    payload
                        .get("tool_call_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    payload
                        .get("tool_name")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    "Approval required for tool call.",
                ));
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
            Some(RunEvent::tool_call_completed(
                run_id,
                trace_id,
                agent_name,
                None,
                payload
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                payload
                    .get("tool_name")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                status,
            ))
        }
        "run_completed" => Some(RunEvent::run_completed(
            run_id,
            trace_id,
            agent_name,
            crate::types::AgentStatus::Completed,
        )),
        "run_wait_user" => Some(RunEvent::run_completed(
            run_id,
            trace_id,
            agent_name,
            crate::types::AgentStatus::WaitUser,
        )),
        "run_max_cycles" => Some(RunEvent::run_completed(
            run_id,
            trace_id,
            agent_name,
            crate::types::AgentStatus::MaxCycles,
        )),
        "cycle_failed" => Some(RunEvent::run_failed(
            run_id,
            trace_id,
            agent_name,
            crate::events::AgentErrorPayload::new(
                payload
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("cycle failed"),
            ),
        )),
        _ => None,
    }
}
