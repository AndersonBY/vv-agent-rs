fn update_host_record(
    transaction: &Transaction<'_>,
    record: &HostInteractionRecord,
) -> CheckpointResult<()> {
    let response = record
        .response
        .as_ref()
        .map(|response| serde_json::to_string(&response.to_value()))
        .transpose()?;
    transaction
        .execute(
            "UPDATE host_interaction_records SET state = ?1, attempt = ?2, claim_token = ?3, lease_expires_at_ms = ?4, response = ?5, response_digest = ?6, command_id = ?7, resolved_revision = ?8, consumed_revision = ?9, last_error = ?10 WHERE record_id = ?11 AND checkpoint_key = ?12 AND interaction_id = ?13",
            params![
                record.state,
                to_i64(record.attempt, "attempt")?,
                record.claim_token,
                record.lease_expires_at_ms.map(|value| to_i64(value, "lease_expires_at_ms")).transpose()?,
                response,
                record.response_digest,
                record.command_id,
                record.resolved_revision.map(|value| to_i64(value, "resolved_revision")).transpose()?,
                record.consumed_revision.map(|value| to_i64(value, "consumed_revision")).transpose()?,
                record.last_error,
                record.record_id,
                record.checkpoint_key,
                record.interaction_id,
            ],
        )
        .map_err(sqlite_error)?;
    Ok(())
}

fn sqlite_append_control_event(
    checkpoint: &mut Checkpoint,
    command_id: &str,
    payload: RunEventPayload,
) -> CheckpointResult<()> {
    sqlite_append_control_event_with_completion(checkpoint, command_id, payload, None)
}

fn sqlite_append_control_event_with_result(
    checkpoint: &mut Checkpoint,
    command_id: &str,
    payload: RunEventPayload,
    result: &AgentResult,
) -> CheckpointResult<()> {
    sqlite_append_control_event_with_completion(checkpoint, command_id, payload, Some(result))
}

fn sqlite_append_control_event_with_completion(
    checkpoint: &mut Checkpoint,
    command_id: &str,
    payload: RunEventPayload,
    result: Option<&AgentResult>,
) -> CheckpointResult<()> {
    let cycle_index = u32::try_from(checkpoint.cycle_index)
        .ok()
        .filter(|cycle| *cycle > 0);
    let event_kind = match &payload {
        RunEventPayload::RunStateChanged { .. } => "run_state_changed",
        RunEventPayload::RunCancelled { .. } => "run_cancelled",
        RunEventPayload::RunFailed { .. } => "run_failed",
        _ => "control",
    };
    let mut event = RunEvent::new(
        checkpoint.root_run_id.clone(),
        checkpoint.trace_id.clone(),
        "vv-agent",
        cycle_index,
        payload,
    );
    if let Some(result) = result {
        event = event
            .with_completion_details(
                result.completion_reason,
                result.completion_tool_name.as_deref(),
                result.partial_output.as_deref(),
            )
            .with_budget_details(result.budget_usage.as_ref(), result.budget_exhaustion.as_ref());
        if let Some(error_code) = result.error_code.as_deref() {
            event
                .metadata
                .insert("error_code".to_string(), serde_json::Value::String(error_code.to_string()));
        }
    }
    event.event_id = EventId::stable(format!("controller-{command_id}-{event_kind}"))
        .map_err(|error| CheckpointError::new("event_identity_conflict", error))?;
    let event_value = serde_json::to_value(&event)
        .map_err(|error| CheckpointError::new("checkpoint_event_invalid", error.to_string()))?;
    checkpoint
        .event_outbox
        .push(crate::runtime::state::EventOutboxEntry::pending(
            event.event_id.as_str(),
            event_value,
        )?);
    Ok(())
}

fn sqlite_control_result(
    checkpoint: &Checkpoint,
    reason: CompletionReason,
    error: &str,
    code: Option<&str>,
) -> AgentResult {
    let mut result = AgentResult::failed(error);
    result.messages = checkpoint.messages.clone();
    result.cycles = checkpoint.cycles.clone();
    result.shared_state = checkpoint.shared_state.clone();
    result.budget_usage = checkpoint.budget_usage.clone();
    result.checkpoint_key = Some(checkpoint.checkpoint_key.clone());
    result.completion_reason = Some(reason);
    result.error_code = code.map(str::to_string);
    result.token_usage =
        crate::runtime::token_usage::summarize_task_token_usage(&checkpoint.model_calls);
    result
}

fn sqlite_claim_and_consume_host_interaction_response(
    store: &SqliteCheckpointStore,
    envelope: HostInteractionRecoveryEnvelope,
) -> CheckpointResult<HostInteractionRecoveryResult> {
    envelope.validate()?;
    let mut connection = store.lock()?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    let current =
        load_row_transaction(&transaction, &envelope.checkpoint_key)?.ok_or_else(|| {
            CheckpointError::new(
                "host_interaction_recovery_stale",
                "checkpoint does not exist",
            )
        })?;
    let record = load_host_record_by_interaction(
        &transaction,
        &envelope.checkpoint_key,
        &envelope.interaction_id,
    )?
        .ok_or_else(|| {
            CheckpointError::new(
                "host_interaction_recovery_stale",
                "host interaction record does not exist",
            )
        })?;
    if record.record_id != envelope.record_id {
        return Err(CheckpointError::new(
            "host_interaction_recovery_stale",
            "record identity does not match envelope",
        ));
    }
    if record.state == "consumed" {
        if !sqlite_recovery_identity_matches(&current, &record, &envelope) {
            return Err(CheckpointError::new(
                "host_interaction_recovery_stale",
                "recovery envelope does not match consumed record",
            ));
        }
        let result = HostInteractionRecoveryResult {
            schema_version: crate::checkpoint::HOST_INTERACTION_RECOVERY_RESULT_SCHEMA.to_string(),
            kind: "replayed".to_string(),
            record_id: record.record_id,
            checkpoint_revision: Some(current.revision),
            consumed_revision: record.consumed_revision,
            claim_mode: "recovery".to_string(),
            resume_attempt: Some(current.resume_attempt),
            injection_count: 1,
            checkpoint_execution_claim_state: if current.claim_token.is_some() {
                "retained".to_string()
            } else {
                "released".to_string()
            },
            error: None,
        };
        result.validate()?;
        transaction.commit().map_err(sqlite_error)?;
        return Ok(result);
    }
    if !sqlite_recovery_identity_matches(&current, &record, &envelope)
        || current.revision != envelope.expected_revision
        || current.resume_attempt != envelope.resume_attempt
        || current.status != crate::checkpoint::CheckpointStatus::Running
        || current.claim_token.is_some()
        || current.has_ambiguous_operation()
        || record.state != "resolved_pending"
    {
        return Err(CheckpointError::new(
            "host_interaction_recovery_stale",
            "recovery envelope is stale or hard recovery barrier is not admissible",
        ));
    }
    let response = record.response.clone().ok_or_else(|| {
        CheckpointError::new(
            "host_interaction_recovery_stale",
            "resolved record has no response",
        )
    })?;
    let claimed_cycle = current
        .cycle_index
        .checked_add(1)
        .ok_or_else(|| CheckpointError::new("checkpoint_cycle_invalid", "cycle index overflow"))?;
    if claimed_cycle != envelope.logical_cycle {
        return Err(CheckpointError::new(
            "host_interaction_recovery_stale",
            "logical cycle does not match checkpoint",
        ));
    }
    let mut updated = current.clone();
    updated.messages.push(crate::types::Message::user(
        response.response.content.clone(),
    ));
    updated.resume_attempt = envelope.resume_attempt.checked_add(1).ok_or_else(|| {
        CheckpointError::new(
            "checkpoint_resume_attempt_overflow",
            "resume attempt overflow",
        )
    })?;
    updated.claim_token = Some(format!(
        "host-recovery:{}:{}",
        record.record_id, updated.resume_attempt
    ));
    updated.claimed_cycle = Some(claimed_cycle);
    updated.lease_expires_at_ms = Some(sqlite_recovery_lease_deadline());
    updated.revision = envelope
        .expected_revision
        .checked_add(1)
        .ok_or_else(|| CheckpointError::new("checkpoint_revision_overflow", "revision overflow"))?;
    let cycle_index = u32::try_from(envelope.logical_cycle).map_err(|_| {
        CheckpointError::new(
            "host_interaction_recovery_stale",
            "logical cycle does not fit RunEvent",
        )
    })?;
    let mut event = RunEvent::new(
        current.root_run_id.clone(),
        current.trace_id.clone(),
        "vv-agent",
        Some(cycle_index.saturating_sub(1)),
        RunEventPayload::HostInteractionResponseConsumed {
            checkpoint_key: current.checkpoint_key.clone(),
            resume_attempt: updated.resume_attempt,
            interaction_id: record.interaction_id.clone(),
            logical_cycle: record.logical_cycle,
            operation_id: record.request.operation_id.clone(),
            tool_call_id: record.request.tool_call_id.clone(),
            request_digest: record.request_digest.clone(),
            command_id: response.command_id.clone(),
            response_digest: response.response_digest.clone(),
            consumed_revision: updated.revision,
        },
    );
    event.event_id = EventId::stable(format!(
        "host-interaction-response-consumed-{}",
        record.record_id
    ))
    .map_err(|error| CheckpointError::new("event_identity_conflict", error))?;
    let event_value = serde_json::to_value(&event)
        .map_err(|error| CheckpointError::new("checkpoint_event_invalid", error.to_string()))?;
    updated
        .event_outbox
        .push(crate::runtime::state::EventOutboxEntry::pending(
            event.event_id.as_str(),
            event_value,
        )?);
    updated.validate()?;
    let values = SqlValues::from_checkpoint(&updated)?;
    if !update_row(&transaction, &values, Some(current.revision), None)? {
        return Err(CheckpointError::new(
            "checkpoint_revision_conflict",
            "host recovery lost the checkpoint CAS",
        ));
    }
    let mut consumed = record;
    consumed.state = "consumed".to_string();
    consumed.consumed_revision = Some(updated.revision);
    consumed.claim_token = None;
    consumed.lease_expires_at_ms = None;
    consumed.validate()?;
    update_host_record(&transaction, &consumed)?;
    let result = HostInteractionRecoveryResult {
        schema_version: crate::checkpoint::HOST_INTERACTION_RECOVERY_RESULT_SCHEMA.to_string(),
        kind: "applied".to_string(),
        record_id: consumed.record_id,
        checkpoint_revision: Some(updated.revision),
        consumed_revision: consumed.consumed_revision,
        claim_mode: "recovery".to_string(),
        resume_attempt: Some(updated.resume_attempt),
        injection_count: 1,
        checkpoint_execution_claim_state: "retained".to_string(),
        error: None,
    };
    result.validate()?;
    transaction.commit().map_err(sqlite_error)?;
    Ok(result)
}

fn sqlite_recovery_identity_matches(
    checkpoint: &Checkpoint,
    record: &HostInteractionRecord,
    envelope: &HostInteractionRecoveryEnvelope,
) -> bool {
    checkpoint.checkpoint_key == envelope.checkpoint_key
        && checkpoint.root_run_id == envelope.run_id
        && checkpoint.trace_id == envelope.trace_id
        && record.checkpoint_key == envelope.checkpoint_key
        && record.logical_cycle == envelope.logical_cycle
        && record.interaction_id == envelope.interaction_id
        && record.request.operation_id == envelope.operation_id
        && record.request.tool_call_id == envelope.tool_call_id
        && record.request_digest == envelope.request_digest
        && record.command_id.as_deref() == Some(envelope.command_id.as_str())
}

fn sqlite_recovery_lease_deadline() -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    now.saturating_add(5 * 60 * 1_000)
}
