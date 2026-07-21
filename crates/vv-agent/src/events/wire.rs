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

pub(super) fn validate_tool_lifecycle_wire_fields(
    value: &Value,
    payload: &RunEventPayload,
) -> Result<(), String> {
    let (tool_call_id, tool_name) = match payload {
        RunEventPayload::ToolCallPlanned {
            tool_call_id,
            tool_name,
            arguments,
        }
        | RunEventPayload::ToolCallStarted {
            tool_call_id,
            tool_name,
            arguments,
        } => {
            if !arguments.is_object() {
                return Err("run event tool arguments must be an object".to_string());
            }
            (tool_call_id, tool_name)
        }
        RunEventPayload::ToolCallCompleted {
            tool_call_id,
            tool_name,
            status,
        } => {
            if matches!(status, ToolStatus::Started) {
                return Err("unsupported completed tool status `started`".to_string());
            }
            validate_tool_completion_additions(value)?;
            (tool_call_id, tool_name)
        }
        _ => return Ok(()),
    };
    require_stream_text(tool_call_id, "tool_call_id")?;
    require_stream_text(tool_name, "tool_name")?;
    if let Some(metadata) = value.get("tool_metadata") {
        serde_json::from_value::<crate::tools::ToolMetadata>(metadata.clone())
            .map_err(|error| format!("invalid run event tool_metadata: {error}"))?;
    }
    Ok(())
}

pub(super) fn validate_compaction_wire_fields(
    value: &Value,
    payload: &RunEventPayload,
) -> Result<(), String> {
    match payload {
        RunEventPayload::MemoryCompactStarted { .. } => {
            for field in ["trigger", "reserved_output_source"] {
                if value.get(field).is_some_and(Value::is_null) {
                    return Err(format!("run event {field} must not be null"));
                }
            }
            for field in [
                "configured_threshold",
                "effective_threshold",
                "microcompact_threshold",
                "model_context_window",
                "model_max_output_tokens",
                "reserved_output_tokens",
                "autocompact_buffer_tokens",
            ] {
                if let Some(raw) = value.get(field) {
                    if field == "model_max_output_tokens" && raw.is_null() {
                        continue;
                    }
                    raw.as_u64()
                        .filter(|counter| *counter <= JSON_SAFE_INTEGER_MAX)
                        .ok_or_else(|| {
                            format!(
                                "run event {field} must be a non-negative JSON-safe integer{}",
                                if field == "model_max_output_tokens" {
                                    " or null"
                                } else {
                                    ""
                                }
                            )
                        })?;
                }
            }
        }
        RunEventPayload::MemoryCompactCompleted { .. } => {
            for field in ["mode", "changed"] {
                if value.get(field).is_some_and(Value::is_null) {
                    return Err(format!("run event {field} must not be null"));
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_tool_completion_additions(value: &Value) -> Result<(), String> {
    if let Some(directive) = value.get("directive") {
        serde_json::from_value::<crate::types::ToolDirective>(directive.clone())
            .map_err(|_| "run event directive is invalid".to_string())?;
    }
    if value
        .get("error_code")
        .is_some_and(|value| !value.is_null() && !value.is_string())
    {
        return Err("run event error_code must be a string or null".to_string());
    }
    let execution_started = match value.get("execution_started") {
        Some(Value::Bool(value)) => Some(*value),
        Some(_) => return Err("run event execution_started must be a boolean".to_string()),
        None => None,
    };
    let duration_ms = match value.get("duration_ms") {
        Some(Value::Null) | None => None,
        Some(value) => Some(
            value
                .as_u64()
                .filter(|value| *value <= JSON_SAFE_INTEGER_MAX)
                .ok_or_else(|| {
                    "run event duration_ms must be a non-negative JSON-safe integer or null"
                        .to_string()
                })?,
        ),
    };
    if execution_started == Some(false) && duration_ms.is_some() {
        return Err(
            "run event duration_ms must be null when execution_started is false".to_string(),
        );
    }
    Ok(())
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
        RunEventPayload::ToolCallPlanned { .. } | RunEventPayload::ToolCallStarted { .. } => {
            &["tool_metadata"]
        }
        RunEventPayload::ToolCallCompleted { .. } => &[
            "directive",
            "error_code",
            "execution_started",
            "duration_ms",
            "tool_metadata",
        ],
        RunEventPayload::MemoryCompactStarted { .. } => &[
            "trigger",
            "configured_threshold",
            "effective_threshold",
            "microcompact_threshold",
            "model_context_window",
            "model_max_output_tokens",
            "reserved_output_tokens",
            "reserved_output_source",
            "autocompact_buffer_tokens",
        ],
        RunEventPayload::MemoryCompactCompleted { .. } => &["mode", "changed"],
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
    let mut fields = keys
        .iter()
        .filter_map(|key| {
            object
                .get(*key)
                .cloned()
                .map(|value| ((*key).to_string(), value))
        })
        .collect::<Metadata>();
    if let Some(metadata) = fields.get_mut("tool_metadata") {
        if let Ok(normalized) =
            serde_json::from_value::<crate::tools::ToolMetadata>(metadata.clone())
        {
            *metadata = serde_json::to_value(normalized).expect("tool metadata serializes");
        }
    }
    fields
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
