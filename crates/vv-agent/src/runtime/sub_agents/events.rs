use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::budget::{BudgetExhaustion, BudgetUsageSnapshot};
use crate::runtime::sub_agent_sessions::SubAgentSessionListener;
use crate::runtime::{RuntimeEventHandler, RuntimeLogHandler};
use crate::types::{AgentStatus, SubTaskOutcome, TaskTokenUsage};

use super::types::SubRunLifecycle;

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
const TRUSTED_STREAM_RECEIPT_KEY: &str = "_vv_agent_stream_receipt";
const TRUSTED_STREAM_SEQUENCE_KEY: &str = "_vv_agent_stream_sequence";

pub(super) fn emit_sub_agent_session_event(
    listeners: &Arc<Mutex<BTreeMap<u64, SubAgentSessionListener>>>,
    event: &str,
    payload: &BTreeMap<String, Value>,
) {
    let listeners = listeners
        .lock()
        .expect("sub-agent session listeners poisoned")
        .values()
        .cloned()
        .collect::<Vec<_>>();
    for listener in listeners {
        let _ = catch_unwind(AssertUnwindSafe(|| listener(event, payload)));
    }
}

pub(super) fn enrich_sub_agent_payload(
    payload: &BTreeMap<String, Value>,
    task_id: &str,
    session_id: &str,
    sub_agent_name: &str,
) -> BTreeMap<String, Value> {
    let mut enriched = payload.clone();
    enriched.insert("task_id".to_string(), Value::String(task_id.to_string()));
    enriched.insert(
        "session_id".to_string(),
        Value::String(session_id.to_string()),
    );
    enriched.insert(
        "sub_agent_name".to_string(),
        Value::String(sub_agent_name.to_string()),
    );
    enriched
}

pub(super) fn canonicalize_sub_agent_stream_event(
    payload: &BTreeMap<String, Value>,
    lifecycle: &SubRunLifecycle,
) -> Option<BTreeMap<String, Value>> {
    let event = payload.get("event").and_then(Value::as_str)?;
    let allowed_fields = match event {
        "assistant_delta" => ASSISTANT_DELTA_FIELDS,
        "reasoning_delta" => REASONING_DELTA_FIELDS,
        "tool_call_started" | "tool_call_progress" => TOOL_STREAM_FIELDS,
        _ => return None,
    };
    let mut canonical = payload
        .iter()
        .filter(|(key, _)| allowed_fields.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<BTreeMap<_, _>>();
    canonical.extend(BTreeMap::from([
        ("event".to_string(), Value::String(event.to_string())),
        (
            "agent_name".to_string(),
            Value::String(lifecycle.agent_name.clone()),
        ),
        (
            "child_run_id".to_string(),
            Value::String(lifecycle.run_id.clone()),
        ),
        (
            "child_session_id".to_string(),
            Value::String(lifecycle.session_id.clone()),
        ),
        (
            "parent_run_id".to_string(),
            Value::String(lifecycle.parent_run_id.clone()),
        ),
        (
            "parent_tool_call_id".to_string(),
            Value::String(lifecycle.parent_tool_call_id.clone()),
        ),
        (
            "run_id".to_string(),
            Value::String(lifecycle.run_id.clone()),
        ),
        (
            "session_id".to_string(),
            Value::String(lifecycle.session_id.clone()),
        ),
        (
            "sub_agent_name".to_string(),
            Value::String(lifecycle.agent_name.clone()),
        ),
        (
            "task_id".to_string(),
            Value::String(lifecycle.task_id.clone()),
        ),
        (
            "trace_id".to_string(),
            Value::String(lifecycle.trace_id.clone()),
        ),
    ]));
    Some(canonical)
}

pub(super) fn emit_parent_sub_agent_event(
    parent_log_handler: &Option<RuntimeLogHandler>,
    parent_event_handler: &Option<RuntimeEventHandler>,
    event: &str,
    payload: BTreeMap<String, Value>,
) {
    emit_parent_log_event(parent_log_handler, event, &payload);
    if let Some(handler) = parent_event_handler {
        handler(event, &payload);
    }
}

pub(super) fn emit_parent_sub_agent_stream_event(
    parent_log_handler: &Option<RuntimeLogHandler>,
    parent_event_handler: &Option<RuntimeEventHandler>,
    canonical: &BTreeMap<String, Value>,
    sequence: u64,
) {
    let event = canonical
        .get("event")
        .and_then(Value::as_str)
        .expect("canonical sub-agent stream event");
    let event = format!("sub_agent_{event}");
    let mut public_payload = canonical.clone();
    public_payload.remove("event");

    if let Some(handler) = parent_log_handler {
        if let Ok(mut handler) = handler.lock() {
            let _ = catch_unwind(AssertUnwindSafe(|| handler(&event, &public_payload)));
        }
    }
    if let Some(handler) = parent_event_handler {
        let mut trusted_payload = public_payload;
        trusted_payload.insert(
            TRUSTED_STREAM_RECEIPT_KEY.to_string(),
            Value::String(format!("stream_{}", uuid::Uuid::new_v4().simple())),
        );
        trusted_payload.insert(
            TRUSTED_STREAM_SEQUENCE_KEY.to_string(),
            Value::from(sequence),
        );
        let _ = catch_unwind(AssertUnwindSafe(|| handler(&event, &trusted_payload)));
    }
}

pub(super) fn agent_status_value(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Pending => "pending",
        AgentStatus::Running => "running",
        AgentStatus::WaitUser => "wait_user",
        AgentStatus::Completed => "completed",
        AgentStatus::Failed => "failed",
        AgentStatus::MaxCycles => "max_cycles",
    }
}

pub(super) fn emit_sub_run_started(
    parent_log_handler: &Option<RuntimeLogHandler>,
    parent_event_handler: &Option<RuntimeEventHandler>,
    lifecycle: &SubRunLifecycle,
) -> Result<(), String> {
    let mut payload = BTreeMap::from([
        (
            "child_run_id".to_string(),
            Value::String(lifecycle.run_id.clone()),
        ),
        (
            "trace_id".to_string(),
            Value::String(lifecycle.trace_id.clone()),
        ),
        (
            "parent_tool_call_id".to_string(),
            Value::String(lifecycle.parent_tool_call_id.clone()),
        ),
        (
            "agent_name".to_string(),
            Value::String(lifecycle.agent_name.clone()),
        ),
        (
            "child_session_id".to_string(),
            Value::String(lifecycle.session_id.clone()),
        ),
        (
            "task_id".to_string(),
            Value::String(lifecycle.task_id.clone()),
        ),
        (
            "metadata".to_string(),
            serde_json::json!({
                "model": lifecycle.model,
                "parent_task_id": lifecycle.parent_task_id,
            }),
        ),
    ]);
    insert_nonempty_string(&mut payload, "parent_run_id", &lifecycle.parent_run_id);
    emit_parent_log_event(parent_log_handler, "sub_run_started", &payload);
    emit_trusted_lifecycle_event(parent_event_handler, "sub_run_started", &payload)
}

pub(super) fn emit_sub_run_completed(
    parent_log_handler: &Option<RuntimeLogHandler>,
    parent_event_handler: &Option<RuntimeEventHandler>,
    lifecycle: &SubRunLifecycle,
    outcome: &SubTaskOutcome,
    token_usage: Option<&TaskTokenUsage>,
    budget_usage: Option<&BudgetUsageSnapshot>,
    budget_exhaustion: Option<&BudgetExhaustion>,
) -> Result<(), String> {
    let payload = sub_run_completed_payload(
        lifecycle,
        outcome,
        token_usage,
        budget_usage,
        budget_exhaustion,
    );
    emit_trusted_lifecycle_event(parent_event_handler, "sub_run_completed", &payload)?;
    emit_parent_log_event(parent_log_handler, "sub_run_completed", &payload);
    Ok(())
}

pub(super) fn emit_sub_run_completed_to_log(
    parent_log_handler: &Option<RuntimeLogHandler>,
    lifecycle: &SubRunLifecycle,
    outcome: &SubTaskOutcome,
    token_usage: Option<&TaskTokenUsage>,
    budget_usage: Option<&BudgetUsageSnapshot>,
    budget_exhaustion: Option<&BudgetExhaustion>,
) {
    let payload = sub_run_completed_payload(
        lifecycle,
        outcome,
        token_usage,
        budget_usage,
        budget_exhaustion,
    );
    emit_parent_log_event(parent_log_handler, "sub_run_completed", &payload);
}

fn sub_run_completed_payload(
    lifecycle: &SubRunLifecycle,
    outcome: &SubTaskOutcome,
    token_usage: Option<&TaskTokenUsage>,
    budget_usage: Option<&BudgetUsageSnapshot>,
    budget_exhaustion: Option<&BudgetExhaustion>,
) -> BTreeMap<String, Value> {
    let mut metadata =
        serde_json::Map::from_iter([("cycles".to_string(), Value::from(outcome.cycles as u64))]);
    let error_code = outcome
        .error_code
        .as_deref()
        .or((outcome.status == AgentStatus::Failed).then_some("sub_task_failed"));
    if let Some(error_code) = error_code {
        metadata.insert(
            "error_code".to_string(),
            Value::String(error_code.to_string()),
        );
    }
    let mut payload = BTreeMap::from([
        (
            "child_run_id".to_string(),
            Value::String(lifecycle.run_id.clone()),
        ),
        (
            "trace_id".to_string(),
            Value::String(lifecycle.trace_id.clone()),
        ),
        (
            "parent_tool_call_id".to_string(),
            Value::String(lifecycle.parent_tool_call_id.clone()),
        ),
        (
            "agent_name".to_string(),
            Value::String(lifecycle.agent_name.clone()),
        ),
        (
            "child_session_id".to_string(),
            Value::String(lifecycle.session_id.clone()),
        ),
        (
            "task_id".to_string(),
            Value::String(lifecycle.task_id.clone()),
        ),
        (
            "status".to_string(),
            Value::String(agent_status_value(outcome.status).to_string()),
        ),
        ("metadata".to_string(), Value::Object(metadata)),
    ]);
    if let Some(token_usage) = token_usage {
        payload.insert(
            "token_usage".to_string(),
            serde_json::to_value(token_usage).unwrap_or(Value::Null),
        );
    }
    if let Some(budget_usage) = budget_usage {
        payload.insert(
            "budget_usage".to_string(),
            serde_json::to_value(budget_usage).unwrap_or(Value::Null),
        );
    }
    if let Some(budget_exhaustion) = budget_exhaustion {
        payload.insert(
            "budget_exhaustion".to_string(),
            serde_json::to_value(budget_exhaustion).unwrap_or(Value::Null),
        );
    }
    insert_nonempty_string(&mut payload, "parent_run_id", &lifecycle.parent_run_id);
    insert_optional_string(
        &mut payload,
        "final_output",
        outcome.final_answer.as_deref(),
    );
    insert_optional_string(&mut payload, "wait_reason", outcome.wait_reason.as_deref());
    insert_optional_string(&mut payload, "error", outcome.error.as_deref());
    if let Some(reason) = outcome.completion_reason {
        payload.insert(
            "completion_reason".to_string(),
            Value::String(reason.as_str().to_string()),
        );
    }
    insert_optional_string(
        &mut payload,
        "completion_tool_name",
        outcome.completion_tool_name.as_deref(),
    );
    insert_optional_string(
        &mut payload,
        "partial_output",
        outcome.partial_output.as_deref(),
    );
    payload
}

fn emit_parent_log_event(
    parent_log_handler: &Option<RuntimeLogHandler>,
    event: &str,
    payload: &BTreeMap<String, Value>,
) {
    if let Some(handler) = parent_log_handler {
        if let Ok(mut handler) = handler.lock() {
            let _ = catch_unwind(AssertUnwindSafe(|| handler(event, payload)));
        }
    }
}

fn emit_trusted_lifecycle_event(
    parent_event_handler: &Option<RuntimeEventHandler>,
    event: &str,
    payload: &BTreeMap<String, Value>,
) -> Result<(), String> {
    let Some(handler) = parent_event_handler else {
        return Ok(());
    };
    catch_unwind(AssertUnwindSafe(|| handler(event, payload))).map_err(|payload| {
        format!(
            "Trusted sub-run lifecycle event sink failed while emitting {event}: {}",
            panic_payload_to_string(payload.as_ref())
        )
    })
}

fn panic_payload_to_string(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    "event sink panicked".to_string()
}

fn insert_nonempty_string(payload: &mut BTreeMap<String, Value>, key: &str, value: &str) {
    if !value.trim().is_empty() {
        payload.insert(key.to_string(), Value::String(value.to_string()));
    }
}

fn insert_optional_string(payload: &mut BTreeMap<String, Value>, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        payload.insert(key.to_string(), Value::String(value.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use serde_json::{json, Value};

    use super::{
        emit_parent_sub_agent_event, emit_sub_agent_session_event, emit_sub_run_completed,
    };
    use crate::runner::{map_runtime_event, RuntimeEventContext};
    use crate::runtime::sub_agent_sessions::SubAgentSessionListener;
    use crate::runtime::sub_agents::types::SubRunLifecycle;
    use crate::runtime::RuntimeEventHandler;
    use crate::types::{AgentStatus, SubTaskOutcome, TaskTokenUsage};

    fn lifecycle() -> SubRunLifecycle {
        SubRunLifecycle {
            run_id: "child-run".to_string(),
            trace_id: "trace-contract".to_string(),
            parent_run_id: "parent-run".to_string(),
            parent_tool_call_id: "delegate".to_string(),
            task_id: "child-task".to_string(),
            session_id: "child-session".to_string(),
            agent_name: "researcher".to_string(),
            parent_task_id: "parent-task".to_string(),
            model: "child-model".to_string(),
        }
    }

    fn outcome(status: AgentStatus) -> SubTaskOutcome {
        SubTaskOutcome {
            task_id: "child-task".to_string(),
            agent_name: "researcher".to_string(),
            status,
            session_id: Some("child-session".to_string()),
            final_answer: None,
            wait_reason: None,
            error: None,
            error_code: None,
            completion_reason: None,
            completion_tool_name: None,
            partial_output: None,
            cycles: 0,
            todo_list: Vec::new(),
            resolved: BTreeMap::new(),
        }
    }

    fn completed_payload(
        outcome: &SubTaskOutcome,
        token_usage: Option<&TaskTokenUsage>,
    ) -> BTreeMap<String, Value> {
        let captured = Arc::new(Mutex::new(None));
        let captured_for_handler = captured.clone();
        let handler: RuntimeEventHandler = Arc::new(move |event, payload| {
            if event == "sub_run_completed" {
                *captured_for_handler.lock().expect("captured payload") = Some(payload.clone());
            }
        });
        emit_sub_run_completed(
            &None,
            &Some(handler),
            &lifecycle(),
            outcome,
            token_usage,
            None,
            None,
        )
        .expect("completion sink");
        let payload = captured
            .lock()
            .expect("captured payload")
            .clone()
            .expect("sub-run completion payload");
        payload
    }

    fn mapped_wire(payload: &BTreeMap<String, Value>) -> Value {
        let context =
            RuntimeEventContext::new("parent-run", "parent-trace", "parent", None, "Delegate");
        let event = map_runtime_event("sub_run_completed", payload, &context)
            .expect("mapped sub-run completion");
        serde_json::to_value(event).expect("serialized sub-run completion")
    }

    #[test]
    fn failed_completion_maps_error_code_metadata_without_unavailable_usage() {
        let mut outcome = outcome(AgentStatus::Failed);
        outcome.error = Some("model resolution failed".to_string());
        outcome.error_code = Some("model_resolution_failed".to_string());

        let payload = completed_payload(&outcome, None);

        assert_eq!(payload["metadata"]["cycles"], json!(0));
        assert_eq!(payload["metadata"]["error_code"], "model_resolution_failed");
        assert!(!payload.contains_key("token_usage"));
        let wire = mapped_wire(&payload);
        assert_eq!(wire["metadata"]["error_code"], "model_resolution_failed");
        assert!(wire.get("token_usage").is_none());
    }

    #[test]
    fn completed_run_keeps_explicit_zero_usage() {
        let mut outcome = outcome(AgentStatus::Completed);
        outcome.final_answer = Some("done".to_string());
        outcome.cycles = 1;
        let usage = TaskTokenUsage::default();

        let payload = completed_payload(&outcome, Some(&usage));

        assert_eq!(payload["token_usage"]["total_tokens"], json!(0));
        assert_eq!(payload["token_usage"]["cycles"], json!([]));
        let wire = mapped_wire(&payload);
        assert_eq!(wire["token_usage"]["total_tokens"], json!(0));
        assert_eq!(wire["token_usage"]["cycles"], json!([]));
    }

    #[test]
    fn wait_user_completion_does_not_fabricate_error_code_metadata() {
        let mut outcome = outcome(AgentStatus::WaitUser);
        outcome.wait_reason = Some("Approve dangerous.".to_string());
        outcome.completion_reason = Some(crate::types::CompletionReason::WaitUser);
        outcome.completion_tool_name = Some("dangerous".to_string());
        outcome.partial_output = Some("proposed change".to_string());

        let payload = completed_payload(&outcome, None);

        assert!(payload["metadata"].get("error_code").is_none());
        assert_eq!(payload["completion_reason"], "wait_user");
        assert_eq!(payload["completion_tool_name"], "dangerous");
        assert_eq!(payload["partial_output"], "proposed change");
        let wire = mapped_wire(&payload);
        assert!(wire["metadata"].get("error_code").is_none());
        assert_eq!(wire["completion_reason"], "wait_user");
        assert_eq!(wire["completion_tool_name"], "dangerous");
        assert_eq!(wire["partial_output"], "proposed change");
    }

    #[test]
    fn panicking_session_listener_does_not_block_other_listeners() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let received_for_listener = received.clone();
        let panicking: SubAgentSessionListener =
            Arc::new(|_: &str, _: &BTreeMap<String, Value>| panic!("broken listener"));
        let healthy: SubAgentSessionListener = Arc::new(move |event, _| {
            received_for_listener
                .lock()
                .expect("received events")
                .push(event.to_string());
        });
        let listeners = Arc::new(Mutex::new(BTreeMap::from([(1, panicking), (2, healthy)])));

        emit_sub_agent_session_event(&listeners, "cycle_started", &BTreeMap::new());

        assert_eq!(
            received.lock().expect("received events").as_slice(),
            ["cycle_started"]
        );
    }

    #[test]
    fn panicking_parent_log_handler_does_not_block_parent_event_handler() {
        let log_handler: crate::runtime::RuntimeLogHandler =
            Arc::new(Mutex::new(Box::new(|_, _| panic!("broken log sink"))));
        let received = Arc::new(Mutex::new(Vec::new()));
        let received_for_handler = received.clone();
        let event_handler: RuntimeEventHandler = Arc::new(move |event, _| {
            received_for_handler
                .lock()
                .expect("received parent events")
                .push(event.to_string());
        });

        emit_parent_sub_agent_event(
            &Some(log_handler),
            &Some(event_handler),
            "sub_run_started",
            BTreeMap::new(),
        );

        assert_eq!(
            received.lock().expect("received parent events").as_slice(),
            ["sub_run_started"]
        );
    }
}
