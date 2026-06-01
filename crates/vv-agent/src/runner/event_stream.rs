use std::collections::{HashSet, VecDeque};

use serde_json::Value;
use tokio::sync::broadcast;

use crate::events::{RunEvent, ToolStatus};
use crate::result::RunResult;
use crate::run_handle::SharedRunResult;

pub struct RunEventStream {
    pending: VecDeque<RunEvent>,
    seen_event_ids: HashSet<String>,
    receiver: Option<broadcast::Receiver<RunEvent>>,
    shared_result: Option<SharedRunResult>,
}

impl RunEventStream {
    pub(crate) fn from_live(
        receiver: Option<broadcast::Receiver<RunEvent>>,
        result: Option<SharedRunResult>,
        backlog: Vec<RunEvent>,
    ) -> Self {
        let seen_event_ids = backlog
            .iter()
            .map(|event| event.event_id().as_str().to_string())
            .collect();
        Self {
            pending: VecDeque::from(backlog),
            seen_event_ids,
            receiver,
            shared_result: result,
        }
    }

    pub async fn next(&mut self) -> Option<Result<RunEvent, String>> {
        if let Some(event) = self.pending.pop_front() {
            return Some(Ok(event));
        }
        let receiver = self.receiver.as_mut()?;
        loop {
            match receiver.recv().await {
                Ok(event) => {
                    if self
                        .seen_event_ids
                        .insert(event.event_id().as_str().to_string())
                    {
                        return Some(Ok(event));
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return None,
                Err(broadcast::error::RecvError::Lagged(count)) => {
                    return Some(Err(format!(
                        "run event stream lagged and dropped {count} events"
                    )));
                }
            }
        }
    }

    pub async fn into_result(mut self) -> Result<RunResult, String> {
        if let Some(result) = self.shared_result.take() {
            return result.wait().await;
        }
        Err("stream result already taken".to_string())
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
        "tool_call_started" => Some(RunEvent::tool_call_started(
            run_id,
            trace_id,
            agent_name,
            payload
                .get("cycle")
                .and_then(Value::as_u64)
                .unwrap_or_default() as u32,
            payload
                .get("tool_call_id")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            payload
                .get("tool_name")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            payload
                .get("tool_arguments")
                .cloned()
                .unwrap_or(Value::Null),
        )),
        "approval_requested" => Some(RunEvent::approval_requested(
            run_id,
            trace_id,
            agent_name,
            payload
                .get("request_id")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            payload
                .get("tool_call_id")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            payload
                .get("tool_name")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            payload
                .get("preview")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )),
        "approval_resolved" => Some(RunEvent::new(
            run_id,
            trace_id,
            agent_name,
            payload
                .get("cycle")
                .and_then(Value::as_u64)
                .map(|cycle| cycle as u32),
            crate::events::RunEventPayload::ApprovalResolved {
                request_id: payload
                    .get("request_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
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
                approved: payload
                    .get("approved")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            },
        )),
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
