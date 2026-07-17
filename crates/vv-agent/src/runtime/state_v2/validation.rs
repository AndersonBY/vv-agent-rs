use super::*;

pub fn validate_checkpoint_v2(checkpoint: &CheckpointV2) -> CheckpointResult<()> {
    if checkpoint.schema_version != CHECKPOINT_V2_SCHEMA {
        return Err(CheckpointError::new(
            "checkpoint_schema_unsupported",
            "checkpoint schema_version is unsupported",
        ));
    }
    if checkpoint.run_definition_schema != RUN_DEFINITION_SCHEMA {
        return Err(CheckpointError::new(
            "checkpoint_definition_schema_unsupported",
            "run_definition_schema is missing or unsupported",
        ));
    }
    crate::checkpoint::validate_run_definition(&checkpoint.run_definition)?;
    validate_sha256(&checkpoint.run_definition_digest, "run_definition_digest").map_err(
        |error| CheckpointError::new("checkpoint_definition_digest_invalid", error.message()),
    )?;
    let digest = crate::checkpoint::run_definition_digest(&checkpoint.run_definition)?;
    if digest != checkpoint.run_definition_digest {
        return Err(CheckpointError::new(
            "checkpoint_definition_mismatch",
            "run_definition_digest does not match embedded run_definition",
        ));
    }
    validate_checkpoint_key(&checkpoint.checkpoint_key)?;
    for (value, field_name) in [
        (&checkpoint.task_id, "task_id"),
        (&checkpoint.root_run_id, "root_run_id"),
        (&checkpoint.trace_id, "trace_id"),
    ] {
        if value.trim().is_empty() {
            return Err(CheckpointError::new(
                "checkpoint_value_invalid",
                format!("{field_name} must be non-empty"),
            ));
        }
    }
    if checkpoint.resume_attempt == 0 || checkpoint.resume_attempt > MAX_WIRE_INTEGER {
        return Err(CheckpointError::new(
            "checkpoint_resume_attempt_invalid",
            "resume_attempt must be positive and JSON-safe",
        ));
    }
    if checkpoint.cycle_index > MAX_WIRE_INTEGER || checkpoint.revision > MAX_WIRE_INTEGER {
        return Err(CheckpointError::new(
            "checkpoint_integer_invalid",
            "checkpoint integer is outside the JSON-safe range",
        ));
    }
    let claim_values = [
        checkpoint.claim_token.is_some(),
        checkpoint.claimed_cycle.is_some(),
        checkpoint.lease_expires_at_ms.is_some(),
    ];
    if claim_values.iter().any(|value| *value) && claim_values.iter().any(|value| !*value) {
        return Err(CheckpointError::new(
            "checkpoint_claim_invalid",
            "claim fields must be all present or all null",
        ));
    }
    if let Some(claim_token) = &checkpoint.claim_token {
        if claim_token.trim().is_empty() {
            return Err(CheckpointError::new(
                "checkpoint_claim_invalid",
                "claim_token must be non-empty",
            ));
        }
        let claimed_cycle = checkpoint.claimed_cycle.expect("claim tuple checked");
        let expected = checkpoint.cycle_index.checked_add(1).ok_or_else(|| {
            CheckpointError::new("checkpoint_claim_invalid", "claimed cycle overflow")
        })?;
        if claimed_cycle != expected || claimed_cycle == 0 || claimed_cycle > MAX_WIRE_INTEGER {
            return Err(CheckpointError::new(
                "checkpoint_claim_invalid",
                "claimed_cycle must equal cycle_index + 1",
            ));
        }
        let lease = checkpoint.lease_expires_at_ms.expect("claim tuple checked");
        if lease > MAX_WIRE_INTEGER {
            return Err(CheckpointError::new(
                "checkpoint_claim_invalid",
                "lease expiry is outside the JSON-safe range",
            ));
        }
    }
    if checkpoint.terminal_result.is_some() && checkpoint.claim_token.is_some() {
        return Err(CheckpointError::new(
            "checkpoint_status_invalid",
            "terminal checkpoint cannot have an active claim",
        ));
    }
    if checkpoint.terminal_acknowledged && checkpoint.terminal_result.is_none() {
        return Err(CheckpointError::new(
            "checkpoint_status_invalid",
            "terminal acknowledgement requires a terminal result",
        ));
    }
    if checkpoint.terminal_result.is_none()
        && !matches!(
            checkpoint.status,
            CheckpointStatus::Running | CheckpointStatus::ReconciliationRequired
        )
    {
        return Err(CheckpointError::new(
            "checkpoint_status_invalid",
            "non-terminal checkpoint must be running or reconciliation_required",
        ));
    }
    if checkpoint.terminal_result.is_some() && !checkpoint.status.is_terminal() {
        return Err(CheckpointError::new(
            "checkpoint_status_invalid",
            "terminal_result requires a terminal checkpoint status",
        ));
    }
    let active_cycle = checkpoint.active_cycle()?;
    for entry in checkpoint
        .model_call_journal
        .iter()
        .chain(checkpoint.tool_journal.iter())
    {
        entry.validate()?;
        if entry.cycle_index != active_cycle {
            return Err(CheckpointError::new(
                "checkpoint_journal_cycle_invalid",
                "journal cycle_index must equal the active cycle",
            ));
        }
    }
    if checkpoint
        .model_call_journal
        .iter()
        .any(|entry| entry.kind != OperationKind::Model)
        || checkpoint
            .tool_journal
            .iter()
            .any(|entry| entry.kind != OperationKind::Tool)
    {
        return Err(CheckpointError::new(
            "checkpoint_journal_kind_invalid",
            "journal arrays contain an entry of the wrong kind",
        ));
    }
    for (namespace, entry) in &checkpoint.extension_state {
        validate_extension_namespace(namespace)?;
        entry.validate()?;
    }
    validate_extension_state_size(&checkpoint.extension_state, MAX_WIRE_INTEGER)?;
    if let Some(cursor) = &checkpoint.event_cursor {
        cursor.validate()?;
    }
    for entry in &checkpoint.event_outbox {
        entry.validate()?;
    }
    for value in checkpoint.shared_state.values() {
        validate_json(value, "shared_state")?;
    }
    for (name, value) in &checkpoint.unknown_fields {
        if known_checkpoint_field(name) {
            return Err(CheckpointError::new(
                "checkpoint_unknown_field_invalid",
                format!("known field {name} cannot be stored as unknown"),
            ));
        }
        validate_json(value, &format!("unknown field {name}"))?;
    }
    if checkpoint.status == CheckpointStatus::ReconciliationRequired
        && (!checkpoint.has_ambiguous_operation() || checkpoint.claim_token.is_some())
    {
        return Err(CheckpointError::new(
            "checkpoint_status_invalid",
            "reconciliation_required needs an ambiguous journal and no claim",
        ));
    }
    if checkpoint.status == CheckpointStatus::Running
        && checkpoint.has_ambiguous_operation()
        && checkpoint.claim_token.is_none()
    {
        return Err(CheckpointError::new(
            "checkpoint_status_invalid",
            "running checkpoint with ambiguity needs an active recovery claim",
        ));
    }
    if checkpoint.terminal_result.is_some()
        && (!checkpoint.model_call_journal.is_empty() || !checkpoint.tool_journal.is_empty())
        && !checkpoint.is_operator_abort_terminal()
    {
        return Err(CheckpointError::new(
            "checkpoint_status_invalid",
            "terminal checkpoint cannot retain active journals",
        ));
    }
    if let Some(result) = &checkpoint.terminal_result {
        validate_json(result, "terminal_result")?;
        let result_status = result.get("status").and_then(Value::as_str);
        if result_status != Some(checkpoint.status.as_str()) {
            return Err(CheckpointError::new(
                "checkpoint_status_invalid",
                "terminal result status must match checkpoint status",
            ));
        }
    }
    Ok(())
}

pub fn validate_extension_state_size(
    extensions: &BTreeMap<String, ExtensionStateEntry>,
    max_total: u64,
) -> CheckpointResult<()> {
    let mut total = 0_u64;
    for (namespace, entry) in extensions {
        let bytes = canonical_json_bytes(&entry.to_value(), "extension state entry")?;
        if bytes.len() > MAX_EXTENSION_ENTRY_BYTES {
            return Err(CheckpointError::new(
                "checkpoint_extension_entry_too_large",
                format!("extension {namespace} exceeds {MAX_EXTENSION_ENTRY_BYTES} bytes"),
            ));
        }
        total = total.checked_add(bytes.len() as u64).ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_extension_state_too_large",
                "extension state byte count overflow",
            )
        })?;
    }
    if total > max_total {
        return Err(CheckpointError::new(
            "checkpoint_extension_state_too_large",
            format!("extension state exceeds {max_total} bytes"),
        ));
    }
    Ok(())
}

pub(super) fn validate_json(value: &Value, field_name: &str) -> CheckpointResult<()> {
    crate::checkpoint::canonical_json_bytes(value, field_name).map(|_| ())
}

pub(super) fn required_string<'a>(
    object: &'a Map<String, Value>,
    field: &str,
    code: &str,
) -> CheckpointResult<&'a str> {
    object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| CheckpointError::new(code, format!("{field} must be a string")))
}

pub(super) fn optional_string(
    object: &Map<String, Value>,
    field: &str,
) -> CheckpointResult<Option<String>> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(CheckpointError::new(
            "operation_kind_fields_invalid",
            format!("{field} must be a string or null"),
        )),
    }
}

pub(super) fn required_u64(
    object: &Map<String, Value>,
    field: &str,
    code: &str,
) -> CheckpointResult<u64> {
    object
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| CheckpointError::new(code, format!("{field} must be a JSON-safe integer")))
}

fn known_checkpoint_field(field: &str) -> bool {
    matches!(
        field,
        "schema_version"
            | "run_definition_schema"
            | "run_definition"
            | "checkpoint_key"
            | "task_id"
            | "root_run_id"
            | "trace_id"
            | "run_definition_digest"
            | "resume_attempt"
            | "cycle_index"
            | "status"
            | "messages"
            | "cycles"
            | "shared_state"
            | "budget_usage"
            | "event_cursor"
            | "event_outbox"
            | "extension_state"
            | "model_call_journal"
            | "tool_journal"
            | "revision"
            | "claim_token"
            | "claimed_cycle"
            | "lease_expires_at_ms"
            | "terminal_result"
            | "terminal_acknowledged"
    )
}
