use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;

use serde_json::Value;
use tokio::sync::broadcast;

use crate::events::{AgentErrorPayload, ApprovalAction, RunEvent, RunEventPayload, ToolStatus};
use crate::result::RunResult;
use crate::run_handle::{active_sub_run_ids, SharedRunResult};

const TRUSTED_STREAM_RECEIPT_KEY: &str = "_vv_agent_stream_receipt";
const TRUSTED_STREAM_SEQUENCE_KEY: &str = "_vv_agent_stream_sequence";
const MAX_PENDING_STREAM_RECEIPTS: usize = 256;
const CANONICAL_STREAM_IDENTITY_FIELDS: &[&str] = &[
    "agent_name",
    "child_run_id",
    "child_session_id",
    "parent_run_id",
    "parent_tool_call_id",
    "run_id",
    "session_id",
    "sub_agent_name",
    "task_id",
    "trace_id",
];
const ASSISTANT_DELTA_FIELDS: &[&str] = &[
    "content_chars",
    "content_delta",
    "delta",
    "estimated_tokens",
    "event",
];
const REASONING_DELTA_FIELDS: &[&str] = &[
    "estimated_tokens",
    "event",
    "reasoning_chars",
    "reasoning_delta",
];
const TOOL_STREAM_FIELDS: &[&str] = &[
    "arguments_chars",
    "estimated_tokens",
    "event",
    "function_name",
    "tool_call_id",
    "tool_call_index",
];

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
        "tool_call_started" => Some(RunEvent::tool_call_started(
            &context.run_id,
            &context.trace_id,
            &context.agent_name,
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
                .get("arguments")
                .or_else(|| payload.get("tool_arguments"))
                .cloned()
                .unwrap_or(Value::Null),
        )),
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
            );
            if let Some(session_id) = child_session_id {
                event = event.with_session_id(session_id);
            }
            Some(with_nested_payload_metadata(event, payload))
        }
        "tool_result" => {
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
                let status = match payload
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_ascii_lowercase()
                    .as_str()
                {
                    "error" => ToolStatus::Error,
                    "wait_response" => ToolStatus::WaitResponse,
                    _ => ToolStatus::Success,
                };
                Some(RunEvent::tool_call_completed(
                    &context.run_id,
                    &context.trace_id,
                    &context.agent_name,
                    payload
                        .get("cycle")
                        .and_then(Value::as_u64)
                        .map(|cycle| cycle as u32),
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
            ),
        ),
        "run_cancelled" => Some(RunEvent::new(
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
        )),
        "run_max_cycles" => Some(RunEvent::run_failed(
            &context.run_id,
            &context.trace_id,
            &context.agent_name,
            AgentErrorPayload::new(
                payload
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("run_max_cycles"),
            ),
        )),
        "run_failed" | "cycle_failed" => Some(RunEvent::run_failed(
            &context.run_id,
            &context.trace_id,
            &context.agent_name,
            AgentErrorPayload::new(
                payload
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("cycle failed"),
            ),
        )),
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
        );
        let event = if handoff_payload || typed_metadata_payload {
            mapped_event
        } else {
            with_payload_metadata(mapped_event, payload)
        };
        context.attach(event)
    })
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

pub(super) fn map_stream_event(
    payload: &std::collections::BTreeMap<String, Value>,
    context: &RuntimeEventContext,
) -> Option<RunEvent> {
    let event = payload
        .get("event")
        .or_else(|| payload.get("type"))
        .and_then(Value::as_str)?;
    if let Some(canonical) = canonical_sub_agent_stream_payload(event, payload) {
        if context.consume_trusted_stream_receipt(&canonical) {
            return None;
        }
    }
    match event {
        "assistant_delta" => Some(
            context.attach(RunEvent::assistant_delta(
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
        ),
        _ => None,
    }
}

fn map_canonical_sub_agent_stream_event(
    stream_event: &str,
    canonical: &BTreeMap<String, Value>,
) -> Option<RunEvent> {
    let payload = match stream_event {
        "assistant_delta" => RunEventPayload::AssistantDelta {
            delta: canonical
                .get("delta")
                .or_else(|| canonical.get("content_delta"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        },
        // RunEvent v1 has one typed text-delta envelope. Preserve the exact
        // producer event and fields in metadata for reasoning stream consumers.
        "reasoning_delta" => RunEventPayload::AssistantDelta {
            delta: canonical
                .get("reasoning_delta")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        },
        // The v1 tool envelope has no progress variant. The canonical metadata
        // remains the authoritative started/progress discriminator.
        "tool_call_started" | "tool_call_progress" => RunEventPayload::ToolCallStarted {
            tool_call_id: canonical
                .get("tool_call_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            tool_name: canonical
                .get("function_name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            arguments: Value::Null,
        },
        _ => return None,
    };
    let mut event = RunEvent::new(
        canonical.get("run_id")?.as_str()?,
        canonical.get("trace_id")?.as_str()?,
        canonical.get("agent_name")?.as_str()?,
        None,
        payload,
    )
    .with_session_id(canonical.get("session_id")?.as_str()?)
    .with_parent_run_id(canonical.get("parent_run_id")?.as_str()?);
    for (key, value) in canonical {
        event = event.with_metadata(key, value.clone());
    }
    Some(event)
}

fn canonical_sub_agent_stream_payload(
    event: &str,
    payload: &BTreeMap<String, Value>,
) -> Option<BTreeMap<String, Value>> {
    let producer_fields = match event {
        "assistant_delta" => ASSISTANT_DELTA_FIELDS,
        "reasoning_delta" => REASONING_DELTA_FIELDS,
        "tool_call_started" | "tool_call_progress" => TOOL_STREAM_FIELDS,
        _ => return None,
    };
    let mut canonical = payload
        .iter()
        .filter(|(key, _)| {
            producer_fields.contains(&key.as_str())
                || CANONICAL_STREAM_IDENTITY_FIELDS.contains(&key.as_str())
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<BTreeMap<_, _>>();
    canonical.insert("event".to_string(), Value::String(event.to_string()));
    if !CANONICAL_STREAM_IDENTITY_FIELDS
        .iter()
        .all(|key| canonical.get(*key).is_some_and(Value::is_string))
    {
        return None;
    }
    for (left, right) in [
        ("run_id", "child_run_id"),
        ("session_id", "child_session_id"),
        ("agent_name", "sub_agent_name"),
    ] {
        if canonical.get(left) != canonical.get(right) {
            return None;
        }
    }
    Some(canonical)
}

fn canonical_stream_fingerprint(payload: &BTreeMap<String, Value>) -> Option<String> {
    serde_json::to_string(payload).ok()
}

fn valid_stream_receipt(marker: &str) -> bool {
    marker.strip_prefix("stream_").is_some_and(|value| {
        value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

fn agent_status(payload: &std::collections::BTreeMap<String, Value>) -> crate::types::AgentStatus {
    match payload
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("completed")
        .to_ascii_lowercase()
        .as_str()
    {
        "pending" => crate::types::AgentStatus::Pending,
        "running" => crate::types::AgentStatus::Running,
        "wait_user" | "wait_response" => crate::types::AgentStatus::WaitUser,
        "failed" | "error" => crate::types::AgentStatus::Failed,
        "max_cycles" => crate::types::AgentStatus::MaxCycles,
        _ => crate::types::AgentStatus::Completed,
    }
}

#[cfg(test)]
mod tests;
