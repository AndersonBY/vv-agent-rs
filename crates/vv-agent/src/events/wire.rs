use super::*;
use crate::checkpoint::{OperationKind, OperationState};

const JSON_SAFE_INTEGER_MAX: u64 = (1_u64 << 53) - 1;

const COMMON_FIELDS: &[&str] = &[
    "version",
    "type",
    "event_id",
    "run_id",
    "trace_id",
    "created_at",
    "session_id",
    "parent_event_id",
    "parent_run_id",
    "cycle_index",
    "agent_name",
    "metadata",
];

pub(super) fn validate_event_wire_shape(value: &Value) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or_else(|| "run event payload must be an object".to_string())?;
    let event_type = object
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| "run event type must be a non-empty string".to_string())?;
    let (event_fields, required_fields): (&[&str], &[&str]) = match event_type {
        "run_started" => (&["input"], &["input"]),
        "agent_started" | "cycle_started" | "session_persisted" => (&[], &[]),
        "llm_started" => (&["model"], &["model"]),
        "run_state_changed" => (&["state"], &["state"]),
        "diagnostic" => (&["level", "code", "details"], &["level", "code", "details"]),
        "assistant_delta" => (
            &["delta", "content_chars", "estimated_tokens"],
            &["delta", "cycle_index"],
        ),
        "reasoning_delta" => (
            &["delta", "reasoning_chars", "estimated_tokens"],
            &["delta", "cycle_index"],
        ),
        "model_tool_call_started" | "model_tool_call_progress" => (
            &[
                "tool_call_id",
                "tool_call_index",
                "tool_name",
                "arguments_chars",
                "estimated_tokens",
            ],
            &["tool_call_id", "tool_name", "cycle_index"],
        ),
        "tool_call_planned" | "tool_call_started" => (
            &["tool_call_id", "tool_name", "arguments", "tool_metadata"],
            &["tool_call_id", "tool_name", "arguments"],
        ),
        "tool_call_completed" => (
            &[
                "tool_call_id",
                "tool_name",
                "status",
                "directive",
                "error_code",
                "execution_started",
                "duration_ms",
                "tool_metadata",
            ],
            &[
                "tool_call_id",
                "tool_name",
                "status",
                "directive",
                "error_code",
                "execution_started",
                "duration_ms",
            ],
        ),
        "approval_requested" => (
            &["request_id", "tool_call_id", "tool_name", "message"],
            &["request_id", "tool_call_id", "tool_name", "message"],
        ),
        "approval_resolved" => (
            &["request_id", "tool_call_id", "tool_name", "action"],
            &["request_id", "tool_call_id", "tool_name", "action"],
        ),
        "memory_compact_started" => (
            &[
                "message_count",
                "estimated_tokens",
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
            &[
                "message_count",
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
        ),
        "memory_compact_completed" => (
            &[
                "before_count",
                "after_count",
                "summary_tokens",
                "mode",
                "changed",
            ],
            &["before_count", "after_count", "mode", "changed"],
        ),
        "sub_run_started" => (
            &[
                "parent_tool_call_id",
                "status",
                "child_session_id",
                "task_id",
            ],
            &["parent_tool_call_id", "status"],
        ),
        "sub_run_completed" => (
            &[
                "parent_tool_call_id",
                "status",
                "child_session_id",
                "task_id",
                "final_output",
                "wait_reason",
                "error",
                "completion_reason",
                "completion_tool_name",
                "partial_output",
                "token_usage",
                "budget_usage",
                "budget_exhaustion",
            ],
            &["parent_tool_call_id", "status"],
        ),
        "handoff_started" => (
            &[
                "source_agent",
                "target_agent",
                "tool_call_id",
                "status",
                "child_session_id",
            ],
            &["source_agent", "target_agent", "tool_call_id", "status"],
        ),
        "handoff_completed" => (
            &[
                "source_agent",
                "target_agent",
                "tool_call_id",
                "status",
                "child_session_id",
                "child_run_id",
            ],
            &["source_agent", "target_agent", "tool_call_id", "status"],
        ),
        "budget_snapshot" => (
            &["enforcement_boundary", "budget_usage"],
            &["enforcement_boundary", "budget_usage"],
        ),
        "budget_exhausted" => (
            &["enforcement_boundary", "budget_usage", "budget_exhaustion"],
            &["enforcement_boundary", "budget_usage", "budget_exhaustion"],
        ),
        "checkpoint_created" | "checkpoint_resumed" => (
            &["checkpoint_key", "resume_attempt"],
            &["checkpoint_key", "resume_attempt", "cycle_index"],
        ),
        "operation_replayed" => (
            &[
                "checkpoint_key",
                "operation_id",
                "operation_kind",
                "receipt_state",
            ],
            &[
                "checkpoint_key",
                "operation_id",
                "operation_kind",
                "receipt_state",
                "cycle_index",
            ],
        ),
        "operation_ambiguous" => (
            &[
                "checkpoint_key",
                "operation_id",
                "operation_kind",
                "risk",
                "idempotency_support",
            ],
            &[
                "checkpoint_key",
                "operation_id",
                "operation_kind",
                "risk",
                "idempotency_support",
                "cycle_index",
            ],
        ),
        "reconciliation_required" => (
            &[
                "checkpoint_key",
                "operation_id",
                "operation_kind",
                "interruption_reason",
                "resume_observation",
            ],
            &[
                "checkpoint_key",
                "operation_id",
                "operation_kind",
                "interruption_reason",
                "resume_observation",
                "cycle_index",
            ],
        ),
        "model_retry_duplicate_risk" => (
            &["checkpoint_key", "operation_id", "operation_kind", "risk"],
            &[
                "checkpoint_key",
                "operation_id",
                "operation_kind",
                "risk",
                "cycle_index",
            ],
        ),
        "reconciliation_resolved" => (
            &[
                "checkpoint_key",
                "operation_id",
                "operation_kind",
                "decision",
            ],
            &[
                "checkpoint_key",
                "operation_id",
                "operation_kind",
                "decision",
                "cycle_index",
            ],
        ),
        "run_completed" => (
            &[
                "status",
                "final_output",
                "completion_reason",
                "completion_tool_name",
                "partial_output",
                "budget_usage",
                "budget_exhaustion",
            ],
            &["status"],
        ),
        "run_failed" => (
            &[
                "error",
                "status",
                "completion_reason",
                "completion_tool_name",
                "partial_output",
                "budget_usage",
                "budget_exhaustion",
            ],
            &["error"],
        ),
        "run_cancelled" => (
            &[
                "reason",
                "completion_reason",
                "partial_output",
                "budget_usage",
                "budget_exhaustion",
            ],
            &["reason"],
        ),
        other => return Err(format!("unsupported run event type `{other}`")),
    };

    let unknown = object
        .keys()
        .filter(|field| {
            !COMMON_FIELDS.contains(&field.as_str()) && !event_fields.contains(&field.as_str())
        })
        .cloned()
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        return Err(format!(
            "run event {event_type} contains unknown fields: {}",
            unknown.join(", ")
        ));
    }
    let mut required = vec![
        "version",
        "type",
        "event_id",
        "run_id",
        "trace_id",
        "created_at",
    ];
    required.extend_from_slice(required_fields);
    let missing = required
        .into_iter()
        .filter(|field| !object.contains_key(*field))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(format!(
            "run event {event_type} is missing required fields: {}",
            missing.join(", ")
        ));
    }
    Ok(())
}

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
            execution_started,
            duration_ms,
            ..
        } => {
            if matches!(status, ToolStatus::Started) {
                return Err("unsupported completed tool status `started`".to_string());
            }
            if duration_ms.is_some_and(|value| value > JSON_SAFE_INTEGER_MAX) {
                return Err(
                    "run event duration_ms must be a non-negative JSON-safe integer or null"
                        .to_string(),
                );
            }
            if !execution_started && duration_ms.is_some() {
                return Err(
                    "run event duration_ms must be null when execution_started is false"
                        .to_string(),
                );
            }
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
    _value: &Value,
    payload: &RunEventPayload,
) -> Result<(), String> {
    match payload {
        RunEventPayload::MemoryCompactStarted {
            configured_threshold,
            effective_threshold,
            microcompact_threshold,
            model_context_window,
            model_max_output_tokens,
            reserved_output_tokens,
            autocompact_buffer_tokens,
            ..
        } => {
            for (field, counter) in [
                ("configured_threshold", *configured_threshold),
                ("effective_threshold", *effective_threshold),
                ("microcompact_threshold", *microcompact_threshold),
                ("model_context_window", *model_context_window),
                ("reserved_output_tokens", *reserved_output_tokens),
                ("autocompact_buffer_tokens", *autocompact_buffer_tokens),
            ] {
                if counter > JSON_SAFE_INTEGER_MAX {
                    return Err(format!(
                        "run event {field} must be a non-negative JSON-safe integer"
                    ));
                }
            }
            if model_max_output_tokens.is_some_and(|counter| counter > JSON_SAFE_INTEGER_MAX) {
                return Err(
                    "run event model_max_output_tokens must be a non-negative JSON-safe integer or null"
                        .to_string(),
                );
            }
        }
        RunEventPayload::MemoryCompactCompleted { .. } => {}
        _ => {}
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
        RunEventPayload::ToolCallCompleted { .. } => &["tool_metadata"],
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

pub(super) fn add_constructed_supplemental_fields(
    payload: &RunEventPayload,
    fields: &mut Metadata,
) {
    let (key, value) = match payload {
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
