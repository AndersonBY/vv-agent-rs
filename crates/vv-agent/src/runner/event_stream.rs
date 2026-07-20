mod budget_events;
mod payload;
mod stream_projection;

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;

use serde_json::Value;
use tokio::sync::broadcast;

use crate::events::{AgentErrorPayload, ApprovalAction, RunEvent, RunEventPayload, ToolStatus};
use crate::result::RunResult;
use crate::run_handle::{active_sub_run_ids, SharedRunResult};
use crate::tools::ToolMetadata;

use payload::{agent_status, completion_reason_from_payload};
pub(super) use stream_projection::map_stream_event;
use stream_projection::{canonical_sub_agent_stream_payload, map_canonical_sub_agent_stream_event};

const TRUSTED_STREAM_RECEIPT_KEY: &str = "_vv_agent_stream_receipt";
const TRUSTED_STREAM_SEQUENCE_KEY: &str = "_vv_agent_stream_sequence";
const MAX_PENDING_STREAM_RECEIPTS: usize = 256;

#[derive(Debug)]
struct TrustedStreamReceipt {
    marker: String,
    sequence: u64,
    fingerprint: String,
    thread_id: ThreadId,
}

#[derive(Debug, Default)]
struct TrustedStreamReceipts {
    pending: VecDeque<TrustedStreamReceipt>,
}

#[doc(hidden)]
#[derive(Clone, Debug)]
pub struct RuntimeEventContext {
    run_id: String,
    trace_id: String,
    agent_name: String,
    session_id: Option<String>,
    input: String,
    trusted_stream_receipts: Arc<Mutex<TrustedStreamReceipts>>,
    observed_tool_completions: Arc<Mutex<HashSet<String>>>,
}

impl RuntimeEventContext {
    pub fn new(
        run_id: impl Into<String>,
        trace_id: impl Into<String>,
        agent_name: impl Into<String>,
        session_id: Option<String>,
        input: impl Into<String>,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            trace_id: trace_id.into(),
            agent_name: agent_name.into(),
            session_id,
            input: input.into(),
            trusted_stream_receipts: Arc::new(Mutex::new(TrustedStreamReceipts::default())),
            observed_tool_completions: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    #[doc(hidden)]
    pub fn map_stream_payload(&self, payload: &BTreeMap<String, Value>) -> Option<RunEvent> {
        map_stream_event(payload, self)
    }

    fn attach(&self, event: RunEvent) -> RunEvent {
        if event.session_id().is_some() {
            return event;
        }
        match &self.session_id {
            Some(session_id) => event.with_session_id(session_id),
            None => event,
        }
    }

    fn register_trusted_stream_receipt(
        &self,
        payload: &BTreeMap<String, Value>,
        canonical: &BTreeMap<String, Value>,
    ) -> bool {
        let Some(marker) = payload
            .get(TRUSTED_STREAM_RECEIPT_KEY)
            .and_then(Value::as_str)
            .filter(|marker| valid_stream_receipt(marker))
        else {
            return false;
        };
        let Some(sequence) = payload
            .get(TRUSTED_STREAM_SEQUENCE_KEY)
            .and_then(Value::as_u64)
            .filter(|sequence| *sequence > 0)
        else {
            return false;
        };
        let Some(fingerprint) = canonical_stream_fingerprint(canonical) else {
            return false;
        };
        let Ok(mut receipts) = self.trusted_stream_receipts.lock() else {
            return false;
        };
        if receipts
            .pending
            .iter()
            .any(|receipt| receipt.marker == marker && receipt.sequence == sequence)
        {
            return false;
        }
        while receipts.pending.len() >= MAX_PENDING_STREAM_RECEIPTS {
            receipts.pending.pop_front();
        }
        receipts.pending.push_back(TrustedStreamReceipt {
            marker: marker.to_string(),
            sequence,
            fingerprint,
            thread_id: std::thread::current().id(),
        });
        true
    }

    fn consume_trusted_stream_receipt(&self, canonical: &BTreeMap<String, Value>) -> bool {
        let Some(fingerprint) = canonical_stream_fingerprint(canonical) else {
            return false;
        };
        let thread_id = std::thread::current().id();
        let Ok(mut receipts) = self.trusted_stream_receipts.lock() else {
            return false;
        };
        let Some(index) = receipts.pending.iter().position(|receipt| {
            receipt.thread_id == thread_id && receipt.fingerprint == fingerprint
        }) else {
            return false;
        };
        receipts.pending.remove(index).is_some()
    }
}

pub struct RunEventStream {
    events: Arc<Mutex<Vec<RunEvent>>>,
    next_index: usize,
    receiver: Option<broadcast::Receiver<RunEvent>>,
    shared_result: Option<SharedRunResult>,
    completion: tokio::sync::watch::Receiver<bool>,
}

impl RunEventStream {
    pub(crate) fn from_live(
        receiver: Option<broadcast::Receiver<RunEvent>>,
        result: Option<SharedRunResult>,
        events: Arc<Mutex<Vec<RunEvent>>>,
        completion: tokio::sync::watch::Receiver<bool>,
    ) -> Self {
        Self {
            events,
            next_index: 0,
            receiver,
            shared_result: result,
            completion,
        }
    }

    pub async fn next(&mut self) -> Option<Result<RunEvent, String>> {
        loop {
            if let Some(event) = self.next_journal_event() {
                return Some(Ok(event));
            }
            if *self.completion.borrow() && self.active_sub_runs().is_empty() {
                return None;
            }
            match self.receiver.as_mut() {
                Some(receiver) => {
                    tokio::select! {
                        event = receiver.recv() => {
                            if matches!(event, Err(broadcast::error::RecvError::Closed)) {
                                self.receiver = None;
                            }
                        },
                        _ = self.completion.changed() => {},
                    }
                }
                None => {
                    if self.completion.changed().await.is_err() {
                        return self.next_journal_event().map(Ok);
                    }
                }
            }
        }
    }

    fn next_journal_event(&mut self) -> Option<RunEvent> {
        let event = self
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(self.next_index)
            .cloned();
        if event.is_some() {
            self.next_index += 1;
        }
        event
    }

    fn active_sub_runs(&self) -> std::collections::HashSet<String> {
        let events = self
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        active_sub_run_ids(&events)
    }

    pub async fn into_result(mut self) -> Result<RunResult, String> {
        if let Some(result) = self.shared_result.take() {
            return result.wait().await;
        }
        Err("stream result already taken".to_string())
    }
}

#[doc(hidden)]
pub fn map_runtime_event(
    event: &str,
    payload: &std::collections::BTreeMap<String, Value>,
    context: &RuntimeEventContext,
) -> Option<RunEvent> {
    let mapped = match event {
        "sub_agent_assistant_delta"
        | "sub_agent_reasoning_delta"
        | "sub_agent_tool_call_started"
        | "sub_agent_tool_call_progress" => {
            let stream_event = event.strip_prefix("sub_agent_")?;
            let canonical = canonical_sub_agent_stream_payload(stream_event, payload)?;
            if !context.register_trusted_stream_receipt(payload, &canonical) {
                return None;
            }
            map_canonical_sub_agent_stream_event(stream_event, &canonical)
        }
        "run_started" => Some(RunEvent::run_started(
            &context.run_id,
            &context.trace_id,
            &context.agent_name,
            &context.input,
        )),
        "cycle_started" => Some(RunEvent::cycle_started(
            &context.run_id,
            &context.trace_id,
            &context.agent_name,
            payload
                .get("cycle")
                .and_then(Value::as_u64)
                .unwrap_or_default() as u32,
        )),
        "agent_started" => Some(RunEvent::new(
            &context.run_id,
            &context.trace_id,
            &context.agent_name,
            payload
                .get("cycle")
                .and_then(Value::as_u64)
                .map(|cycle| cycle as u32),
            RunEventPayload::AgentStarted,
        )),
        "llm_started" => Some(RunEvent::new(
            &context.run_id,
            &context.trace_id,
            &context.agent_name,
            payload
                .get("cycle")
                .and_then(Value::as_u64)
                .map(|cycle| cycle as u32),
            RunEventPayload::LlmStarted {
                model: payload
                    .get("model")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            },
        )),
        "run_state_changed" => Some(RunEvent::new(
            &context.run_id,
            &context.trace_id,
            &context.agent_name,
            payload
                .get("cycle")
                .and_then(Value::as_u64)
                .map(|cycle| cycle as u32),
            RunEventPayload::RunStateChanged {
                state: payload
                    .get("state")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            },
        )),
        "session_persisted" => Some(RunEvent::new(
            &context.run_id,
            &context.trace_id,
            &context.agent_name,
            payload
                .get("cycle")
                .and_then(Value::as_u64)
                .map(|cycle| cycle as u32),
            RunEventPayload::SessionPersisted,
        )),
        "budget_snapshot" => budget_events::map_budget_snapshot(payload, context),
        "budget_exhausted" => budget_events::map_budget_exhausted(payload, context),
        "assistant_delta" => Some(RunEvent::assistant_delta(
            &context.run_id,
            &context.trace_id,
            &context.agent_name,
            payload
                .get("cycle")
                .and_then(Value::as_u64)
                .unwrap_or_default() as u32,
            payload
                .get("delta")
                .or_else(|| payload.get("content_delta"))
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )),
        // This is a complete cycle record, not a streaming token delta. The v1
        // typed payload has no cycle-response variant, so keep it out of the
        // assistant_delta channel instead of duplicating the full answer.
        "cycle_llm_response" => None,
        "tool_call_planned" => map_runtime_tool_call(payload, context, true),
        "tool_call_started" => map_runtime_tool_call(payload, context, false),
        "tool_call_completed" => {
            let event = map_runtime_tool_completion(payload, context)?;
            if let Ok(mut observed) = context.observed_tool_completions.lock() {
                observed.insert(tool_completion_key(payload));
            }
            Some(event)
        }
        "approval_requested" => {
            let tool_name = payload_string(payload, "tool_name");
            Some(with_selected_payload_metadata(
                RunEvent::new(
                    &context.run_id,
                    &context.trace_id,
                    &context.agent_name,
                    payload
                        .get("cycle")
                        .and_then(Value::as_u64)
                        .map(|cycle| cycle as u32),
                    RunEventPayload::ApprovalRequested {
                        request_id: payload_string(payload, "request_id"),
                        tool_call_id: payload_string(payload, "tool_call_id"),
                        tool_name,
                        message: payload
                            .get("message")
                            .or_else(|| payload.get("preview"))
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    },
                ),
                payload,
                &["arguments", "tool_name"],
            ))
        }
        "approval_resolved" => {
            let approved = payload
                .get("approved")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let action = payload
                .get("action")
                .and_then(Value::as_str)
                .and_then(ApprovalAction::parse)
                .unwrap_or_else(|| ApprovalAction::from_approved(approved));
            Some(with_selected_payload_metadata(
                RunEvent::new(
                    &context.run_id,
                    &context.trace_id,
                    &context.agent_name,
                    payload
                        .get("cycle")
                        .and_then(Value::as_u64)
                        .map(|cycle| cycle as u32),
                    RunEventPayload::ApprovalResolved {
                        request_id: payload
                            .get("request_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        tool_name: payload
                            .get("tool_name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        tool_call_id: payload
                            .get("tool_call_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        approved: action.is_approved(),
                    },
                )
                .with_approval_action(action),
                payload,
                &["action", "reason", "decision_metadata"],
            ))
        }
        "sub_run_started" => {
            let child_session_id = payload.get("child_session_id").and_then(Value::as_str);
            let mut event = RunEvent::new(
                child_run_id(payload).unwrap_or(&context.run_id),
                payload
                    .get("trace_id")
                    .and_then(Value::as_str)
                    .unwrap_or(&context.trace_id),
                payload
                    .get("agent_name")
                    .and_then(Value::as_str)
                    .unwrap_or(&context.agent_name),
                payload
                    .get("cycle")
                    .and_then(Value::as_u64)
                    .map(|cycle| cycle as u32),
                RunEventPayload::SubRunStarted {
                    parent_tool_call_id: payload_string(payload, "parent_tool_call_id"),
                    child_session_id: child_session_id.map(str::to_string),
                    task_id: payload
                        .get("task_id_hint")
                        .or_else(|| payload.get("task_id"))
                        .and_then(Value::as_str)
                        .map(str::to_string),
                },
            )
            .with_parent_run_id(
                payload
                    .get("parent_run_id")
                    .and_then(Value::as_str)
                    .unwrap_or(&context.run_id),
            );
            if let Some(session_id) = child_session_id {
                event = event.with_session_id(session_id);
            }
            Some(with_nested_payload_metadata(event, payload))
        }
        "sub_run_completed" => {
            let child_session_id = payload.get("child_session_id").and_then(Value::as_str);
            let task_id = (payload.contains_key("child_run_id")
                || payload.contains_key("child_session_id"))
            .then(|| payload.get("task_id").and_then(Value::as_str))
            .flatten()
            .or_else(|| payload.get("task_id_hint").and_then(Value::as_str));
            let mut event = RunEvent::new(
                child_run_id(payload).unwrap_or(&context.run_id),
                payload
                    .get("trace_id")
                    .and_then(Value::as_str)
                    .unwrap_or(&context.trace_id),
                payload
                    .get("agent_name")
                    .and_then(Value::as_str)
                    .unwrap_or(&context.agent_name),
                payload
                    .get("cycle")
                    .and_then(Value::as_u64)
                    .map(|cycle| cycle as u32),
                RunEventPayload::SubRunCompleted {
                    parent_tool_call_id: payload_string(payload, "parent_tool_call_id"),
                    status: agent_status(payload),
                    final_output: payload
                        .get("final_output")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                },
            )
            .with_parent_run_id(
                payload
                    .get("parent_run_id")
                    .and_then(Value::as_str)
                    .unwrap_or(&context.run_id),
            )
            .with_sub_run_details(
                child_session_id,
                task_id,
                payload.get("wait_reason").and_then(Value::as_str),
                payload.get("error").and_then(Value::as_str),
                payload.get("token_usage").cloned(),
            )
            .with_budget_details(
                payload
                    .get("budget_usage")
                    .and_then(|value| serde_json::from_value(value.clone()).ok())
                    .as_ref(),
                payload
                    .get("budget_exhaustion")
                    .and_then(|value| serde_json::from_value(value.clone()).ok())
                    .as_ref(),
            )
            .with_completion_details(
                completion_reason_from_payload(payload, None),
                payload.get("completion_tool_name").and_then(Value::as_str),
                payload.get("partial_output").and_then(Value::as_str),
            );
            if let Some(session_id) = child_session_id {
                event = event.with_session_id(session_id);
            }
            Some(with_nested_payload_metadata(event, payload))
        }
        "tool_result" => {
            if payload
                .get("lifecycle_suppressed")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                return None;
            }
            let metadata = payload.get("metadata").and_then(Value::as_object);
            if let Some(interruption_id) = metadata
                .and_then(|metadata| metadata.get("approval_interruption_id"))
                .and_then(Value::as_str)
            {
                let tool_name = payload_string(payload, "tool_name");
                Some(RunEvent::new(
                    &context.run_id,
                    &context.trace_id,
                    &context.agent_name,
                    payload
                        .get("cycle")
                        .and_then(Value::as_u64)
                        .map(|cycle| cycle as u32),
                    RunEventPayload::ApprovalRequested {
                        request_id: interruption_id.to_string(),
                        tool_call_id: payload_string(payload, "tool_call_id"),
                        tool_name: tool_name.clone(),
                        message: metadata
                            .and_then(|metadata| metadata.get("message"))
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_else(|| format!("Approval required for tool {tool_name}.")),
                    },
                ))
            } else if metadata
                .and_then(|metadata| metadata.get("mode"))
                .and_then(Value::as_str)
                .is_some_and(|mode| mode == "handoff")
            {
                let metadata = metadata.expect("handoff metadata");
                let mut event = RunEvent::new(
                    &context.run_id,
                    &context.trace_id,
                    &context.agent_name,
                    payload
                        .get("cycle")
                        .and_then(Value::as_u64)
                        .map(|cycle| cycle as u32),
                    RunEventPayload::Handoff {
                        source_agent: metadata
                            .get("handoff_from")
                            .and_then(Value::as_str)
                            .unwrap_or(&context.agent_name)
                            .to_string(),
                        target_agent: metadata
                            .get("handoff_to")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        tool_call_id: payload
                            .get("tool_call_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    },
                );
                for (key, value) in metadata {
                    event = event.with_metadata(key, value.clone());
                }
                Some(event)
            } else {
                let already_observed = context
                    .observed_tool_completions
                    .lock()
                    .map(|observed| observed.contains(&tool_completion_key(payload)))
                    .unwrap_or(false);
                (!already_observed)
                    .then(|| map_runtime_tool_completion(payload, context))
                    .flatten()
            }
        }
        "run_completed" => Some(
            RunEvent::new(
                &context.run_id,
                &context.trace_id,
                &context.agent_name,
                payload
                    .get("cycle")
                    .and_then(Value::as_u64)
                    .map(|cycle| cycle as u32),
                RunEventPayload::RunCompleted {
                    status: agent_status(payload),
                },
            )
            .with_final_output(
                payload
                    .get("final_output")
                    .or_else(|| payload.get("final_answer"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
            )
            .with_completion_details(
                completion_reason_from_payload(payload, None),
                payload.get("completion_tool_name").and_then(Value::as_str),
                payload.get("partial_output").and_then(Value::as_str),
            ),
        ),
        "run_wait_user" => Some(
            RunEvent::new(
                &context.run_id,
                &context.trace_id,
                &context.agent_name,
                payload
                    .get("cycle")
                    .and_then(Value::as_u64)
                    .map(|cycle| cycle as u32),
                RunEventPayload::RunCompleted {
                    status: crate::types::AgentStatus::WaitUser,
                },
            )
            .with_final_output(
                payload
                    .get("wait_reason")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            )
            .with_completion_details(
                completion_reason_from_payload(
                    payload,
                    Some(crate::types::CompletionReason::WaitUser),
                ),
                payload.get("completion_tool_name").and_then(Value::as_str),
                payload.get("partial_output").and_then(Value::as_str),
            ),
        ),
        "run_cancelled" => Some(
            RunEvent::new(
                &context.run_id,
                &context.trace_id,
                &context.agent_name,
                None,
                RunEventPayload::RunCancelled {
                    reason: payload
                        .get("reason")
                        .and_then(Value::as_str)
                        .or_else(|| payload.get("error").and_then(Value::as_str))
                        .unwrap_or("run cancelled")
                        .to_string(),
                },
            )
            .with_completion_details(
                completion_reason_from_payload(
                    payload,
                    Some(crate::types::CompletionReason::Cancelled),
                ),
                None,
                payload.get("partial_output").and_then(Value::as_str),
            ),
        ),
        "run_max_cycles" => Some(
            RunEvent::run_failed(
                &context.run_id,
                &context.trace_id,
                &context.agent_name,
                AgentErrorPayload::new(
                    payload
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("run_max_cycles"),
                ),
            )
            .with_completion_details(
                completion_reason_from_payload(
                    payload,
                    Some(crate::types::CompletionReason::MaxCycles),
                ),
                None,
                payload.get("partial_output").and_then(Value::as_str),
            ),
        ),
        "run_failed" | "cycle_failed" => Some(
            RunEvent::run_failed(
                &context.run_id,
                &context.trace_id,
                &context.agent_name,
                AgentErrorPayload::new(
                    payload
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("cycle failed"),
                ),
            )
            .with_completion_details(
                completion_reason_from_payload(
                    payload,
                    Some(crate::types::CompletionReason::Failed),
                ),
                None,
                payload.get("partial_output").and_then(Value::as_str),
            ),
        ),
        _ => None,
    };
    mapped.map(|mapped_event| {
        let handoff_payload = event == "tool_result"
            && payload
                .get("metadata")
                .and_then(Value::as_object)
                .and_then(|metadata| metadata.get("mode"))
                .and_then(Value::as_str)
                == Some("handoff");
        let typed_metadata_payload = matches!(
            event,
            "approval_requested"
                | "approval_resolved"
                | "sub_agent_assistant_delta"
                | "sub_agent_reasoning_delta"
                | "sub_agent_tool_call_started"
                | "sub_agent_tool_call_progress"
                | "sub_run_started"
                | "sub_run_completed"
                | "budget_snapshot"
                | "budget_exhausted"
        );
        let event = if handoff_payload || typed_metadata_payload {
            mapped_event
        } else {
            with_payload_metadata(mapped_event, payload)
        };
        context.attach(event)
    })
}

fn map_runtime_tool_call(
    payload: &BTreeMap<String, Value>,
    context: &RuntimeEventContext,
    planned: bool,
) -> Option<RunEvent> {
    let tool_call_id = payload_string_non_empty(payload, "tool_call_id")?;
    let tool_name = payload_string_non_empty(payload, "tool_name")?;
    let arguments = payload
        .get("arguments")
        .or_else(|| payload.get("tool_arguments"))?
        .clone();
    if !arguments.is_object() {
        return None;
    }
    let tool_metadata = runtime_tool_metadata(payload)?;
    let cycle_index = payload
        .get("cycle")
        .and_then(Value::as_u64)
        .unwrap_or_default() as u32;
    let event = if planned {
        RunEvent::tool_call_planned(
            &context.run_id,
            &context.trace_id,
            &context.agent_name,
            cycle_index,
            tool_call_id,
            tool_name,
            arguments,
        )
    } else {
        RunEvent::tool_call_started(
            &context.run_id,
            &context.trace_id,
            &context.agent_name,
            cycle_index,
            tool_call_id,
            tool_name,
            arguments,
        )
    };
    Some(event.with_tool_metadata(tool_metadata.as_ref()))
}

fn map_runtime_tool_completion(
    payload: &BTreeMap<String, Value>,
    context: &RuntimeEventContext,
) -> Option<RunEvent> {
    const JSON_SAFE_INTEGER_MAX: u64 = (1_u64 << 53) - 1;

    let status = runtime_tool_status(payload.get("status")?.as_str()?)?;
    let tool_call_id = payload_string_non_empty(payload, "tool_call_id")?;
    let tool_name = payload_string_non_empty(payload, "tool_name")?;
    let tool_metadata = runtime_tool_metadata(payload)?;
    let mut event = RunEvent::tool_call_completed(
        &context.run_id,
        &context.trace_id,
        &context.agent_name,
        payload
            .get("cycle")
            .and_then(Value::as_u64)
            .map(|cycle| cycle as u32),
        tool_call_id,
        tool_name,
        status,
    )
    .with_tool_metadata(tool_metadata.as_ref());

    if let Some(value) = payload.get("directive") {
        serde_json::from_value::<crate::types::ToolDirective>(value.clone()).ok()?;
        event = event.with_tool_completion_wire_field("directive", value.clone());
    }
    if let Some(value) = payload.get("error_code") {
        if !value.is_null() && !value.is_string() {
            return None;
        }
        event = event.with_tool_completion_wire_field("error_code", value.clone());
    }
    let execution_started = match payload.get("execution_started") {
        Some(Value::Bool(value)) => {
            event = event.with_tool_completion_wire_field("execution_started", Value::Bool(*value));
            Some(*value)
        }
        Some(_) => return None,
        None => None,
    };
    if let Some(value) = payload.get("duration_ms") {
        let duration = match value {
            Value::Null => None,
            value => Some(
                value
                    .as_u64()
                    .filter(|value| *value <= JSON_SAFE_INTEGER_MAX)?,
            ),
        };
        if execution_started == Some(false) && duration.is_some() {
            return None;
        }
        event = event.with_tool_completion_wire_field("duration_ms", value.clone());
    }
    Some(event)
}

fn runtime_tool_status(status: &str) -> Option<ToolStatus> {
    match status.to_ascii_lowercase().as_str() {
        "success" => Some(ToolStatus::Success),
        "error" => Some(ToolStatus::Error),
        "wait_response" => Some(ToolStatus::WaitResponse),
        "running" => Some(ToolStatus::Running),
        "pending_compress" => Some(ToolStatus::PendingCompress),
        _ => None,
    }
}

fn runtime_tool_metadata(payload: &BTreeMap<String, Value>) -> Option<Option<ToolMetadata>> {
    payload
        .get("tool_metadata")
        .map(|value| serde_json::from_value(value.clone()).ok().map(Some))
        .unwrap_or(Some(None))
}

fn payload_string_non_empty<'a>(
    payload: &'a BTreeMap<String, Value>,
    field: &str,
) -> Option<&'a str> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn tool_completion_key(payload: &BTreeMap<String, Value>) -> String {
    format!(
        "{}\0{}",
        payload
            .get("cycle")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        payload
            .get("tool_call_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
    )
}

fn child_run_id(payload: &std::collections::BTreeMap<String, Value>) -> Option<&str> {
    payload
        .get("child_run_id")
        .or_else(|| payload.get("task_id_hint"))
        .and_then(Value::as_str)
}

fn payload_string(payload: &std::collections::BTreeMap<String, Value>, key: &str) -> String {
    payload
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn with_payload_metadata(
    mut event: RunEvent,
    payload: &std::collections::BTreeMap<String, Value>,
) -> RunEvent {
    for (key, value) in payload {
        event = event.with_metadata(key, value.clone());
    }
    event
}

fn with_nested_payload_metadata(
    mut event: RunEvent,
    payload: &std::collections::BTreeMap<String, Value>,
) -> RunEvent {
    if let Some(metadata) = payload.get("metadata").and_then(Value::as_object) {
        for (key, value) in metadata {
            event = event.with_metadata(key, value.clone());
        }
    }
    event
}

fn with_selected_payload_metadata(
    mut event: RunEvent,
    payload: &std::collections::BTreeMap<String, Value>,
    fields: &[&str],
) -> RunEvent {
    for field in fields {
        if let Some(value) = payload.get(*field) {
            event = event.with_metadata(*field, value.clone());
        }
    }
    event
}

fn canonical_stream_fingerprint(payload: &BTreeMap<String, Value>) -> Option<String> {
    serde_json::to_string(payload).ok()
}

fn valid_stream_receipt(marker: &str) -> bool {
    marker.strip_prefix("stream_").is_some_and(|value| {
        value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

#[cfg(test)]
mod tests;
