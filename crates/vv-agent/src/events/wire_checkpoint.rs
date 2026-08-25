pub(super) fn validate_checkpoint_wire_fields(
    payload: &RunEventPayload,
    cycle_index: Option<u32>,
) -> Result<(), String> {
    let require_lifecycle_cycle =
        || cycle_index.ok_or_else(|| "checkpoint lifecycle event requires cycle_index".to_string());
    match payload {
        RunEventPayload::HostInteractionRequested {
            checkpoint_key,
            resume_attempt,
            interaction_id,
            logical_cycle,
            operation_id,
            tool_call_id,
            request_digest,
            prompt,
        } => {
            let cycle = require_lifecycle_cycle()?;
            for (field, value) in [
                ("checkpoint_key", checkpoint_key),
                ("interaction_id", interaction_id),
                ("operation_id", operation_id),
                ("tool_call_id", tool_call_id),
            ] {
                require_event_text(value, field)?;
            }
            if *resume_attempt == 0 || *logical_cycle == 0 || *logical_cycle > JSON_SAFE_INTEGER_MAX
            {
                return Err("host interaction event integer is invalid".to_string());
            }
            if *logical_cycle != u64::from(cycle).saturating_add(1) {
                return Err(
                    "host interaction event logical_cycle must equal cycle_index + 1".to_string(),
                );
            }
            if prompt.trim().is_empty() || prompt.len() > 65_536 {
                return Err("host interaction event prompt is empty or too large".to_string());
            }
            if crate::checkpoint::sanitize_public_text(prompt) != *prompt {
                return Err("host interaction event prompt is not sanitized".to_string());
            }
            crate::checkpoint::validate_sha256(request_digest, "request_digest")
                .map_err(|error| error.to_string())?;
            let expected = crate::checkpoint::HostInteractionRequest::new(
                interaction_id.clone(),
                *logical_cycle,
                operation_id.clone(),
                tool_call_id.clone(),
                prompt.clone(),
            )
            .map_err(|error| error.to_string())?
            .request_digest;
            if expected != *request_digest {
                return Err(
                    "host interaction event request_digest does not match request".to_string(),
                );
            }
        }
        RunEventPayload::HostInteractionResponseConsumed {
            checkpoint_key,
            resume_attempt,
            interaction_id,
            logical_cycle,
            operation_id,
            tool_call_id,
            request_digest,
            command_id,
            response_digest,
            consumed_revision,
        } => {
            let cycle = require_lifecycle_cycle()?;
            for (field, value) in [
                ("checkpoint_key", checkpoint_key),
                ("interaction_id", interaction_id),
                ("operation_id", operation_id),
                ("tool_call_id", tool_call_id),
                ("command_id", command_id),
            ] {
                require_event_text(value, field)?;
            }
            if *resume_attempt == 0
                || *consumed_revision > JSON_SAFE_INTEGER_MAX
                || *logical_cycle == 0
                || *logical_cycle != u64::from(cycle).saturating_add(1)
            {
                return Err("host interaction consumed event integer is invalid".to_string());
            }
            crate::checkpoint::validate_sha256(request_digest, "request_digest")
                .map_err(|error| error.to_string())?;
            crate::checkpoint::validate_sha256(response_digest, "response_digest")
                .map_err(|error| error.to_string())?;
        }
        RunEventPayload::ToolCallDeferred {
            handle,
            operation_id,
            attempt,
            ..
        } => {
            handle.validate().map_err(|error| error.to_string())?;
            if handle.operation_id != *operation_id || handle.attempt != u64::from(*attempt) {
                return Err(
                    "deferred tool event handle identity does not match operation".to_string(),
                );
            }
        }
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
            decision,
            claim_mode,
            ..
        } => {
            require_lifecycle_cycle()?;
            require_event_operation(checkpoint_key, operation_id)?;
            if *decision == crate::checkpoint::ReconciliationDecisionKind::AcceptDeferred
                && *claim_mode != Some(crate::checkpoint::ClaimMode::Recovery)
            {
                return Err(
                    "accept_deferred reconciliation event requires recovery claim_mode".to_string(),
                );
            }
            if *decision != crate::checkpoint::ReconciliationDecisionKind::AcceptDeferred
                && claim_mode.is_some()
            {
                return Err(
                    "claim_mode is only valid for accept_deferred reconciliation events"
                        .to_string(),
                );
            }
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
