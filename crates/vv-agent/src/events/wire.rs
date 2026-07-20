use super::*;
use crate::checkpoint::{OperationKind, OperationState};

const JSON_SAFE_INTEGER_MAX: u64 = (1_u64 << 53) - 1;

pub(super) fn validate_completion_wire_fields(value: &Value) -> Result<(), String> {
    if let Some(value) = value.get("completion_reason") {
        match value {
            Value::Null => {}
            Value::String(reason) if CompletionReason::parse(reason).is_some() => {}
            Value::String(reason) => {
                return Err(format!(
                    "unsupported run event completion_reason `{reason}`"
                ));
            }
            _ => return Err("run event completion_reason must be a string or null".to_string()),
        }
    }
    for field in ["completion_tool_name", "partial_output"] {
        if value
            .get(field)
            .is_some_and(|value| !value.is_null() && !value.is_string())
        {
            return Err(format!("run event {field} must be a string or null"));
        }
    }
    Ok(())
}

pub(super) fn validate_budget_wire_fields(value: &Value) -> Result<(), String> {
    if let Some(value) = value.get("budget_usage") {
        match value {
            Value::Null => {}
            Value::Object(_) => {
                serde_json::from_value::<BudgetUsageSnapshot>(value.clone())
                    .map_err(|error| format!("invalid run event budget_usage: {error}"))?;
            }
            _ => return Err("run event budget_usage must be an object or null".to_string()),
        }
    }
    if let Some(value) = value.get("budget_exhaustion") {
        match value {
            Value::Null => {}
            Value::Object(_) => {
                serde_json::from_value::<BudgetExhaustion>(value.clone())
                    .map_err(|error| format!("invalid run event budget_exhaustion: {error}"))?;
            }
            _ => return Err("run event budget_exhaustion must be an object or null".to_string()),
        }
    }
    Ok(())
}

pub(super) fn validate_stream_wire_fields(
    payload: &RunEventPayload,
    cycle_index: Option<u32>,
) -> Result<(), String> {
    let require_positive_cycle = || match cycle_index {
        Some(cycle) if cycle > 0 => Ok(()),
        _ => Err("stream event requires a positive cycle_index".to_string()),
    };
    match payload {
        RunEventPayload::AssistantDelta {
            content_chars,
            estimated_tokens,
            ..
        } => {
            require_positive_cycle()?;
            validate_stream_counters(&[
                ("content_chars", *content_chars),
                ("estimated_tokens", *estimated_tokens),
            ])
        }
        RunEventPayload::ReasoningDelta {
            reasoning_chars,
            estimated_tokens,
            ..
        } => {
            require_positive_cycle()?;
            validate_stream_counters(&[
                ("reasoning_chars", *reasoning_chars),
                ("estimated_tokens", *estimated_tokens),
            ])
        }
        RunEventPayload::ModelToolCallStarted {
            tool_call_id,
            tool_call_index,
            tool_name,
            arguments_chars,
            estimated_tokens,
        }
        | RunEventPayload::ModelToolCallProgress {
            tool_call_id,
            tool_call_index,
            tool_name,
            arguments_chars,
            estimated_tokens,
        } => {
            require_positive_cycle()?;
            require_stream_text(tool_call_id, "tool_call_id")?;
            require_stream_text(tool_name, "tool_name")?;
            validate_stream_counters(&[
                ("tool_call_index", *tool_call_index),
                ("arguments_chars", *arguments_chars),
                ("estimated_tokens", *estimated_tokens),
            ])
        }
        _ => Ok(()),
    }
}

fn validate_stream_counters(fields: &[(&str, Option<u64>)]) -> Result<(), String> {
    for (field, value) in fields {
        if value.is_some_and(|value| value > JSON_SAFE_INTEGER_MAX) {
            return Err(format!(
                "run event {field} must be a non-negative JSON-safe integer or null"
            ));
        }
    }
    Ok(())
}

fn require_stream_text(value: &str, field: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("run event {field} must be a non-empty string"));
    }
    Ok(())
}

pub(super) fn validate_checkpoint_wire_fields(
    payload: &RunEventPayload,
    cycle_index: Option<u32>,
) -> Result<(), String> {
    let require_lifecycle_cycle =
        || cycle_index.ok_or_else(|| "checkpoint lifecycle event requires cycle_index".to_string());
    match payload {
        RunEventPayload::CheckpointCreated {
            checkpoint_key,
            resume_attempt,
        }
        | RunEventPayload::CheckpointResumed {
            checkpoint_key,
            resume_attempt,
        } => {
            require_lifecycle_cycle()?;
            require_event_text(checkpoint_key, "checkpoint_key")?;
            if *resume_attempt == 0 {
                return Err("checkpoint lifecycle resume_attempt must be positive".to_string());
            }
        }
        RunEventPayload::OperationReplayed {
            checkpoint_key,
            operation_id,
            receipt_state,
            ..
        } => {
            require_lifecycle_cycle()?;
            require_event_operation(checkpoint_key, operation_id)?;
            if !matches!(
                receipt_state,
                OperationState::Succeeded | OperationState::Failed
            ) {
                return Err(
                    "operation replay receipt_state must be succeeded or failed".to_string()
                );
            }
        }
        RunEventPayload::OperationAmbiguous {
            checkpoint_key,
            operation_id,
            operation_kind,
            risk,
            idempotency_support,
        } => {
            require_lifecycle_cycle()?;
            require_event_operation(checkpoint_key, operation_id)?;
            require_event_text(risk, "risk")?;
            match (operation_kind, idempotency_support) {
                (OperationKind::Tool, Some(_)) | (OperationKind::Model, None) => {}
                (OperationKind::Tool, None) => {
                    return Err("ambiguous tool event requires idempotency_support".to_string());
                }
                (OperationKind::Model, Some(_)) => {
                    return Err(
                        "ambiguous model event idempotency_support must be null".to_string()
                    );
                }
            }
        }
        RunEventPayload::ReconciliationRequired {
            checkpoint_key,
            operation_id,
            operation_kind,
            interruption_reason,
            resume_observation,
        } => {
            let cycle = require_lifecycle_cycle()?;
            require_event_operation(checkpoint_key, operation_id)?;
            require_event_text(interruption_reason, "interruption_reason")?;
            resume_observation
                .validate()
                .map_err(|error| error.to_string())?;
            if resume_observation.operation_id != *operation_id
                || resume_observation.operation_kind != *operation_kind
                || resume_observation.cycle_index != u64::from(cycle)
            {
                return Err(
                    "reconciliation event operation must match resume_observation".to_string(),
                );
            }
        }
        RunEventPayload::ModelRetryDuplicateRisk {
            checkpoint_key,
            operation_id,
            operation_kind,
            risk,
        } => {
            require_lifecycle_cycle()?;
            require_event_operation(checkpoint_key, operation_id)?;
            require_event_text(risk, "risk")?;
            if *operation_kind != OperationKind::Model {
                return Err(
                    "model retry duplicate risk event requires model operation_kind".to_string(),
                );
            }
        }
        RunEventPayload::ReconciliationResolved {
            checkpoint_key,
            operation_id,
            ..
        } => {
            require_lifecycle_cycle()?;
            require_event_operation(checkpoint_key, operation_id)?;
        }
        _ => {}
    }
    Ok(())
}

fn require_event_operation(checkpoint_key: &str, operation_id: &str) -> Result<(), String> {
    require_event_text(checkpoint_key, "checkpoint_key")?;
    require_event_text(operation_id, "operation_id")
}

fn require_event_text(value: &str, field: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("checkpoint lifecycle {field} must be non-empty"));
    }
    Ok(())
}

pub(super) fn supplemental_wire_fields(value: &Value, payload: &RunEventPayload) -> Metadata {
    let Some(object) = value.as_object() else {
        return Metadata::new();
    };
    let keys: &[&str] = match payload {
        RunEventPayload::ApprovalResolved { .. } => &["action"],
        RunEventPayload::SubRunStarted { .. } => &["status"],
        RunEventPayload::SubRunCompleted { .. } => &[
            "child_session_id",
            "task_id",
            "wait_reason",
            "error",
            "completion_reason",
            "completion_tool_name",
            "partial_output",
            "token_usage",
            "budget_usage",
            "budget_exhaustion",
        ],
        RunEventPayload::HandoffStarted { .. } => &["status", "child_session_id"],
        RunEventPayload::HandoffCompleted { .. } => &["status", "child_session_id", "child_run_id"],
        RunEventPayload::RunCompleted { .. } => &[
            "final_output",
            "completion_reason",
            "completion_tool_name",
            "partial_output",
            "budget_usage",
            "budget_exhaustion",
        ],
        RunEventPayload::RunFailed { .. } | RunEventPayload::RunCancelled { .. } => &[
            "status",
            "completion_reason",
            "completion_tool_name",
            "partial_output",
            "budget_usage",
            "budget_exhaustion",
        ],
        _ => &[],
    };
    keys.iter()
        .filter_map(|key| {
            object
                .get(*key)
                .cloned()
                .map(|value| ((*key).to_string(), value))
        })
        .collect()
}

pub(super) fn add_default_supplemental_fields(payload: &RunEventPayload, fields: &mut Metadata) {
    let (key, value) = match payload {
        RunEventPayload::ApprovalResolved { approved, .. } => (
            "action",
            Value::String(
                ApprovalAction::from_approved(*approved)
                    .as_str()
                    .to_string(),
            ),
        ),
        RunEventPayload::SubRunStarted { .. } => ("status", Value::String("running".to_string())),
        RunEventPayload::HandoffStarted { .. } => ("status", Value::String("started".to_string())),
        RunEventPayload::HandoffCompleted { .. } => {
            ("status", Value::String("completed".to_string()))
        }
        RunEventPayload::RunCompleted { .. } => ("final_output", Value::Null),
        _ => return,
    };
    fields.entry(key.to_string()).or_insert(value);
}
