use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::budget::{BudgetExhaustion, BudgetUsageSnapshot};
use crate::events::{DiagnosticLevel, RunEvent, RunEventPayload};
use crate::runtime::sub_agent_sessions::SubAgentSessionListener;
use crate::runtime::RunEventHandler;
use crate::types::{AgentStatus, SubTaskOutcome, TaskTokenUsage};

use super::types::SubRunLifecycle;

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

pub(super) fn emit_parent_sub_agent_event(
    event_handler: &Option<RunEventHandler>,
    code: &str,
    mut payload: BTreeMap<String, Value>,
) {
    let run_id = take_identity(&mut payload, &["child_run_id", "run_id", "task_id"])
        .unwrap_or_else(|| "sub_run".to_string());
    let trace_id = take_identity(&mut payload, &["trace_id"]).unwrap_or_else(|| run_id.clone());
    let agent_name = take_identity(&mut payload, &["agent_name", "sub_agent_name"])
        .unwrap_or_else(|| "sub_agent".to_string());
    let session_id = take_identity(&mut payload, &["child_session_id", "session_id"]);
    let parent_run_id = take_identity(&mut payload, &["parent_run_id"]);
    let cycle_index = payload
        .remove("cycle")
        .or_else(|| payload.remove("cycle_index"))
        .and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0);
    let mut event = RunEvent::diagnostic(
        run_id,
        trace_id,
        agent_name,
        cycle_index,
        DiagnosticLevel::Debug,
        code,
        payload.into_iter().collect(),
    );
    if let Some(session_id) = session_id {
        event = event.with_session_id(session_id);
    }
    if let Some(parent_run_id) = parent_run_id {
        event = event.with_parent_run_id(parent_run_id);
    }
    let _ = emit_typed_event(event_handler, &event);
}

pub(super) fn agent_status_value(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Pending => "pending",
        AgentStatus::Running => "running",
        AgentStatus::WaitUser => "wait_user",
        AgentStatus::Completed => "completed",
        AgentStatus::Failed => "failed",
        AgentStatus::MaxCycles => "max_cycles",
        AgentStatus::ReconciliationRequired => "reconciliation_required",
    }
}

pub(super) fn emit_sub_run_started(
    event_handler: &Option<RunEventHandler>,
    lifecycle: &SubRunLifecycle,
) -> Result<(), String> {
    let mut event = RunEvent::new(
        &lifecycle.run_id,
        &lifecycle.trace_id,
        &lifecycle.agent_name,
        None,
        RunEventPayload::SubRunStarted {
            parent_tool_call_id: lifecycle.parent_tool_call_id.clone(),
            child_session_id: Some(lifecycle.session_id.clone()),
            task_id: Some(lifecycle.task_id.clone()),
        },
    )
    .with_session_id(&lifecycle.session_id)
    .with_parent_run_id(&lifecycle.parent_run_id)
    .with_metadata("model", Value::String(lifecycle.model.clone()))
    .with_metadata(
        "parent_task_id",
        Value::String(lifecycle.parent_task_id.clone()),
    );
    if lifecycle.parent_run_id.trim().is_empty() {
        event = RunEvent::new(
            &lifecycle.run_id,
            &lifecycle.trace_id,
            &lifecycle.agent_name,
            None,
            event.payload().clone(),
        )
        .with_session_id(&lifecycle.session_id)
        .with_metadata("model", Value::String(lifecycle.model.clone()))
        .with_metadata(
            "parent_task_id",
            Value::String(lifecycle.parent_task_id.clone()),
        );
    }
    emit_typed_event(event_handler, &event)
}

pub(super) fn emit_sub_run_completed(
    event_handler: &Option<RunEventHandler>,
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
    let mut event = crate::runner::map_runtime_event(
        "sub_run_completed",
        &payload,
        &crate::runner::RuntimeEventContext::new(
            &lifecycle.run_id,
            &lifecycle.trace_id,
            &lifecycle.agent_name,
            Some(lifecycle.session_id.clone()),
            "",
        ),
    )
    .ok_or_else(|| "failed to build typed sub_run_completed event".to_string())?;
    event = event.with_parent_run_id(&lifecycle.parent_run_id);
    emit_typed_event(event_handler, &event)
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

fn emit_typed_event(
    event_handler: &Option<RunEventHandler>,
    event: &RunEvent,
) -> Result<(), String> {
    if let Some(handler) = event_handler {
        catch_unwind(AssertUnwindSafe(|| handler(event)))
            .map_err(|payload| panic_payload_to_string(payload.as_ref()))?;
    }
    Ok(())
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

fn take_identity(payload: &mut BTreeMap<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        payload
            .remove(*key)
            .and_then(|value| value.as_str().map(str::to_string))
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
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
    use crate::runtime::sub_agent_sessions::SubAgentSessionListener;
    use crate::runtime::sub_agents::types::SubRunLifecycle;
    use crate::runtime::RunEventHandler;
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

    fn completed_payload(outcome: &SubTaskOutcome, token_usage: Option<&TaskTokenUsage>) -> Value {
        let captured = Arc::new(Mutex::new(None));
        let captured_for_handler = captured.clone();
        let handler: RunEventHandler = Arc::new(move |event| {
            if matches!(
                event.payload(),
                crate::events::RunEventPayload::SubRunCompleted { .. }
            ) {
                *captured_for_handler.lock().expect("captured payload") = Some(event.clone());
            }
        });
        emit_sub_run_completed(
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
        serde_json::to_value(payload).expect("serialized sub-run completion")
    }

    #[test]
    fn failed_completion_maps_error_code_metadata_without_unavailable_usage() {
        let mut outcome = outcome(AgentStatus::Failed);
        outcome.error = Some("model resolution failed".to_string());
        outcome.error_code = Some("model_resolution_failed".to_string());

        let payload = completed_payload(&outcome, None);

        assert_eq!(payload["metadata"]["cycles"], json!(0));
        assert_eq!(payload["metadata"]["error_code"], "model_resolution_failed");
        assert!(payload.get("token_usage").is_none());
    }

    #[test]
    fn completed_run_keeps_explicit_zero_usage() {
        let mut outcome = outcome(AgentStatus::Completed);
        outcome.final_answer = Some("done".to_string());
        outcome.cycles = 1;
        let usage = TaskTokenUsage {
            input_tokens: Some(0),
            output_tokens: Some(0),
            total_tokens: Some(0),
            reasoning_tokens: Some(0),
            ..TaskTokenUsage::default()
        };

        let payload = completed_payload(&outcome, Some(&usage));

        assert_eq!(payload["token_usage"]["total_tokens"], json!(0));
        assert_eq!(payload["token_usage"]["model_calls"], json!([]));
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
    fn panicking_parent_event_handler_is_isolated() {
        let event_handler: RunEventHandler = Arc::new(|_| panic!("broken event sink"));

        emit_parent_sub_agent_event(&Some(event_handler), "sub_run_started", BTreeMap::new());
    }
}
