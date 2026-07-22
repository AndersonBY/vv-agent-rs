use std::sync::Arc;

use crate::agent::Agent;
use crate::events::{AgentErrorPayload, RunEvent, RunEventPayload};
use crate::model::{ModelProvider, ModelRef};
use crate::run_config::RunConfig;
use crate::types::{AgentResult, AgentStatus, CompletionReason};

pub(super) fn terminal_event(
    result: &AgentResult,
    run_id: &str,
    trace_id: &str,
    agent_name: &str,
    session_id: Option<&str>,
    cancellation_token: Option<&crate::runtime::CancellationToken>,
) -> RunEvent {
    let cycle_index = u32::try_from(result.cycles.len())
        .ok()
        .filter(|value| *value > 0);
    let cancelled = result.completion_reason == Some(CompletionReason::Cancelled)
        || (result.status == AgentStatus::Failed
            && cancellation_token.is_some_and(crate::runtime::CancellationToken::is_cancelled));
    let mut event = if cancelled {
        RunEvent::new(
            run_id,
            trace_id,
            agent_name,
            cycle_index,
            RunEventPayload::RunCancelled {
                reason: result
                    .error
                    .clone()
                    .or_else(|| cancellation_token.and_then(|token| token.reason()))
                    .unwrap_or_else(|| "run cancelled".to_string()),
            },
        )
    } else if matches!(result.status, AgentStatus::Failed | AgentStatus::MaxCycles) {
        RunEvent::run_failed(
            run_id,
            trace_id,
            agent_name,
            AgentErrorPayload::new(
                result
                    .error
                    .clone()
                    .unwrap_or_else(|| status_string(result.status)),
            ),
        )
    } else {
        RunEvent::new(
            run_id,
            trace_id,
            agent_name,
            cycle_index,
            RunEventPayload::RunCompleted {
                status: result.status,
            },
        )
        .with_final_output(
            result
                .final_answer
                .clone()
                .or_else(|| result.wait_reason.clone())
                .or_else(|| result.error.clone()),
        )
    };
    event = event.with_completion_details(
        result.completion_reason,
        result.completion_tool_name.as_deref(),
        result.partial_output.as_deref(),
    );
    event = event.with_budget_details(
        result.budget_usage.as_ref(),
        result.budget_exhaustion.as_ref(),
    );
    if let Some(session_id) = session_id {
        event = event.with_session_id(session_id);
    }
    event
}

pub(super) fn status_string(status: AgentStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "failed".to_string())
}

pub(super) fn effective_model_ref(
    agent: &Agent,
    runner_defaults: &RunConfig,
    config: &RunConfig,
    provider: &Arc<dyn ModelProvider>,
) -> Option<ModelRef> {
    config
        .model
        .clone()
        .or_else(|| agent.model().cloned())
        .or_else(|| {
            if config.model_provider.is_none() {
                runner_defaults.model.clone()
            } else {
                None
            }
        })
        .or_else(|| provider.default_model_ref())
}

fn normalized_config_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub(super) fn effective_trace_id(default_config: &RunConfig, config: &RunConfig) -> String {
    normalized_config_string(config.trace_id.as_deref())
        .or_else(|| normalized_config_string(default_config.trace_id.as_deref()))
        .unwrap_or_else(|| format!("trace_{}", uuid::Uuid::new_v4().simple()))
}

pub(super) fn effective_workflow_name(
    default_config: &RunConfig,
    config: &RunConfig,
) -> Option<String> {
    normalized_config_string(config.workflow_name.as_deref())
        .or_else(|| normalized_config_string(default_config.workflow_name.as_deref()))
}
