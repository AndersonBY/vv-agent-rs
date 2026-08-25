pub(super) fn supplemental_wire_fields(value: &Value, payload: &RunEventPayload) -> Metadata {
    let Some(object) = value.as_object() else {
        return Metadata::new();
    };
    let keys: &[&str] = match payload {
        RunEventPayload::ToolCallPlanned { .. } | RunEventPayload::ToolCallStarted { .. } => {
            &["tool_metadata"]
        }
        RunEventPayload::ToolCallCompleted { .. } => &["tool_metadata"],
        RunEventPayload::ToolCallDeferred { .. } => &["checkpoint_key"],
        RunEventPayload::ReconciliationResolved { .. } => &["claim_mode"],
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
