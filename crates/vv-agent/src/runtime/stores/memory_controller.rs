fn host_interaction_outcome(
    request: &HostInteractionRequest,
    checkpoint_revision: u64,
    status: &str,
    record_id: &str,
    notification: &HostInteractionNotificationRecord,
) -> CheckpointResult<HostInteractionOutcome> {
    let outcome = HostInteractionOutcome {
        schema_version: crate::checkpoint::HOST_INTERACTION_OUTCOME_SCHEMA.to_string(),
        interaction_id: request.interaction_id.clone(),
        logical_cycle: request.logical_cycle,
        checkpoint_revision,
        status: status.to_string(),
        outbox_state: "pending".to_string(),
        record_id: record_id.to_string(),
        notification_id: notification.notification_id.clone(),
        notification_payload_digest: notification.payload_digest.clone(),
        notification_outbox_action: "host_interaction_notification".to_string(),
        notification_outbox_destination: "host_interaction_observer".to_string(),
    };
    outcome.validate()?;
    Ok(outcome)
}

fn replay_resolution(resolution: &ControllerCommandResolution) -> ControllerCommandResolution {
    match resolution {
        ControllerCommandResolution::Applied { receipt, wake }
        | ControllerCommandResolution::Replayed { receipt, wake } => {
            ControllerCommandResolution::Replayed {
                receipt: receipt.clone(),
                wake: wake.clone(),
            }
        }
        ControllerCommandResolution::Rejected { error } => ControllerCommandResolution::Rejected {
            error: error.clone(),
        },
    }
}

fn recovery_identity_matches(
    checkpoint: &Checkpoint,
    record: &HostInteractionRecord,
    envelope: &HostInteractionRecoveryEnvelope,
) -> bool {
    checkpoint.checkpoint_key == envelope.checkpoint_key
        && checkpoint.root_run_id == envelope.run_id
        && checkpoint.trace_id == envelope.trace_id
        && record.record_id == envelope.record_id
        && record.checkpoint_key == envelope.checkpoint_key
        && record.logical_cycle == envelope.logical_cycle
        && record.interaction_id == envelope.interaction_id
        && record.request.operation_id == envelope.operation_id
        && record.request.tool_call_id == envelope.tool_call_id
        && record.request_digest == envelope.request_digest
        && record.command_id.as_deref() == Some(envelope.command_id.as_str())
}

fn recovery_lease_deadline() -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    now.saturating_add(5 * 60 * 1_000)
}

fn sanitize_public_prompt(prompt: &str) -> String {
    crate::checkpoint::sanitize_public_text(prompt)
}

fn append_control_event(
    checkpoint: &mut Checkpoint,
    command_id: &str,
    payload: RunEventPayload,
) -> CheckpointResult<()> {
    append_control_event_with_completion(checkpoint, command_id, payload, None)
}

fn append_control_event_with_result(
    checkpoint: &mut Checkpoint,
    command_id: &str,
    payload: RunEventPayload,
    result: &AgentResult,
) -> CheckpointResult<()> {
    append_control_event_with_completion(checkpoint, command_id, payload, Some(result))
}

fn append_control_event_with_completion(
    checkpoint: &mut Checkpoint,
    command_id: &str,
    payload: RunEventPayload,
    result: Option<&AgentResult>,
) -> CheckpointResult<()> {
    // Controller events describe the checkpoint that was just committed.  A
    // control transition must never invent the next execution cycle; the
    // distributed worker owns that cycle claim and will emit its own events.
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

fn control_result(
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

fn apply_controller_command(
    checkpoints: &mut BTreeMap<String, Checkpoint>,
    ledger: &mut ControllerLedger,
    command: &ControllerCommand,
) -> CheckpointResult<(ControllerCommandReceipt, ControllerCommandResolution)> {
    let handle = &command.handle;
    let current = checkpoints
        .get(&handle.checkpoint_key)
        .cloned()
        .ok_or_else(|| {
            CheckpointError::new("controller_command_stale", "checkpoint does not exist")
        })?;
    if current.root_run_id != handle.run_id || current.trace_id != handle.trace_id {
        return Err(CheckpointError::new(
            "controller_command_stale",
            "controller handle does not match the authoritative checkpoint",
        ));
    }
    if current.resume_attempt != command.resume_attempt
        || current.revision != command.expected_revision
    {
        return Err(CheckpointError::new(
            "controller_command_stale",
            "controller fence does not match the authoritative checkpoint",
        ));
    }
    if current.terminal_result.is_some() || current.status.is_terminal() {
        return Err(CheckpointError::new(
            "controller_command_terminal",
            "controller commands cannot mutate a terminal checkpoint",
        ));
    }
    if current.claim_token.is_some() {
        return Err(CheckpointError::new(
            "controller_command_claim_active",
            "controller commands require a released execution claim",
        ));
    }
    if current.has_ambiguous_operation()
        && !matches!(&command.command, ControllerCommandVariant::Abort)
    {
        return Err(CheckpointError::new(
            "controller_command_ambiguity_requires_reconciliation",
            "controller command is blocked by an ambiguous external operation",
        ));
    }
    if current.status == crate::checkpoint::CheckpointStatus::Deferred {
        return Err(CheckpointError::new(
            "controller_command_deferred_pending",
            "deferred resolution is an authoritative barrier",
        ));
    }

    let mut updated = current.clone();
    let mut wake = ControllerCommandWake::none();
    match &command.command {
        ControllerCommandVariant::HostInteractionResponse {
            interaction_id,
            logical_cycle,
            operation_id,
            tool_call_id,
            request_digest,
            response,
        } => {
            let pending = match (
                &current.status,
                &current.active_host_interaction,
                &current.suspended_origin,
            ) {
                (crate::checkpoint::CheckpointStatus::HostInteraction, Some(request), _) => {
                    Some(request.clone())
                }
                (crate::checkpoint::CheckpointStatus::Suspended, _, Some(origin))
                    if origin.status == "host_interaction" =>
                {
                    origin.active_host_interaction.clone()
                }
                _ => None,
            }
            .ok_or_else(|| {
                CheckpointError::new(
                    "controller_command_invalid_state",
                    "no pending host interaction matches response",
                )
            })?;
            if pending.interaction_id != *interaction_id
                || pending.logical_cycle != *logical_cycle
                || pending.operation_id != *operation_id
                || pending.tool_call_id != *tool_call_id
                || pending.request_digest != *request_digest
            {
                return Err(CheckpointError::new(
                    "controller_command_stale",
                    "host response identity does not match pending interaction",
                ));
            }
            let record_id = record_id_for(&current.checkpoint_key, &pending);
            let mut record = ledger
                .host_interactions
                .get(&record_id)
                .cloned()
                .ok_or_else(|| {
                    CheckpointError::new(
                        "controller_command_stale",
                        "pending host interaction record does not exist",
                    )
                })?;
            if record.state != "active" || record.request != pending {
                return Err(CheckpointError::new(
                    "controller_command_stale",
                    "host interaction record is no longer active",
                ));
            }
            let resolved = HostInteractionResponse::new(
                interaction_id.clone(),
                *logical_cycle,
                operation_id.clone(),
                tool_call_id.clone(),
                request_digest.clone(),
                command.command_id.clone(),
                response.clone(),
            )?;
            record.state = "resolved_pending".to_string();
            record.response = Some(resolved.clone());
            record.response_digest = Some(resolved.response_digest.clone());
            record.command_id = Some(command.command_id.clone());
            record.resolved_revision = Some(current.revision + 1);
            record.validate()?;
            if current.status == crate::checkpoint::CheckpointStatus::HostInteraction {
                updated.status = crate::checkpoint::CheckpointStatus::Running;
                updated.active_host_interaction = None;
                wake = ControllerCommandWake::recovery(*logical_cycle);
            } else {
                updated.status = crate::checkpoint::CheckpointStatus::Suspended;
                updated.active_host_interaction = None;
                updated.suspended_origin = Some(SuspendedOrigin::host_interaction(pending));
            }
            updated.revision = current.revision + 1;
            updated.validate()?;
            ledger.host_interactions.insert(record_id, record);
        }
        ControllerCommandVariant::Suspend => {
            let origin = match current.status {
                crate::checkpoint::CheckpointStatus::Running => SuspendedOrigin::running(),
                crate::checkpoint::CheckpointStatus::HostInteraction => {
                    SuspendedOrigin::host_interaction(
                        current.active_host_interaction.clone().ok_or_else(|| {
                            CheckpointError::new(
                                "controller_command_invalid_state",
                                "host interaction status has no request",
                            )
                        })?,
                    )
                }
                _ => {
                    return Err(CheckpointError::new(
                        "controller_command_invalid_state",
                        "suspend is not valid in the current state",
                    ))
                }
            };
            updated.status = crate::checkpoint::CheckpointStatus::Suspended;
            updated.active_host_interaction = None;
            updated.suspended_origin = Some(origin);
            updated.revision = current.revision + 1;
            append_control_event(
                &mut updated,
                &command.command_id,
                RunEventPayload::RunStateChanged {
                    state: "suspended".to_string(),
                },
            )?;
            updated.validate()?;
        }
        ControllerCommandVariant::Resume => {
            if current.status != crate::checkpoint::CheckpointStatus::Suspended {
                return Err(CheckpointError::new(
                    "controller_command_invalid_state",
                    "resume requires suspended state",
                ));
            }
            let origin = current.suspended_origin.clone().ok_or_else(|| {
                CheckpointError::new(
                    "controller_command_invalid_state",
                    "suspended checkpoint has no origin",
                )
            })?;
            match origin.status.as_str() {
                "running" => {
                    updated.status = crate::checkpoint::CheckpointStatus::Running;
                    updated.suspended_origin = None;
                    wake = ControllerCommandWake::recovery(current.cycle_index.saturating_add(1));
                }
                "host_interaction" => {
                    let request = origin.active_host_interaction.clone().ok_or_else(|| {
                        CheckpointError::new(
                            "controller_command_invalid_state",
                            "host origin has no request",
                        )
                    })?;
                    let record_id = record_id_for(&current.checkpoint_key, &request);
                    let pending_response = ledger
                        .host_interactions
                        .get(&record_id)
                        .is_some_and(|record| record.state == "resolved_pending");
                    if pending_response {
                        updated.status = crate::checkpoint::CheckpointStatus::Running;
                        updated.suspended_origin = None;
                        wake = ControllerCommandWake::recovery(request.logical_cycle);
                    } else {
                        updated.status = crate::checkpoint::CheckpointStatus::HostInteraction;
                        updated.active_host_interaction = Some(request);
                        updated.suspended_origin = None;
                    }
                }
                _ => {
                    return Err(CheckpointError::new(
                        "controller_command_invalid_state",
                        "unsupported suspended origin",
                    ))
                }
            }
            updated.revision = current.revision + 1;
            let resulting_state = updated.status.as_str().to_string();
            append_control_event(
                &mut updated,
                &command.command_id,
                RunEventPayload::RunStateChanged {
                    state: resulting_state,
                },
            )?;
            updated.validate()?;
        }
        ControllerCommandVariant::Cancel => {
            let result = control_result(
                &current,
                CompletionReason::Cancelled,
                "Operation was cancelled",
                Some("cancelled"),
            );
            updated.status = crate::checkpoint::CheckpointStatus::Failed;
            updated.active_host_interaction = None;
            updated.suspended_origin = None;
            updated.claim_token = None;
            updated.claimed_cycle = None;
            updated.lease_expires_at_ms = None;
            updated.terminal_result = Some(result.to_dict());
            updated.revision = current.revision + 1;
            append_control_event(
                &mut updated,
                &command.command_id,
                RunEventPayload::RunStateChanged {
                    state: "failed".to_string(),
                },
            )?;
            append_control_event_with_result(
                &mut updated,
                &command.command_id,
                RunEventPayload::RunCancelled {
                    reason: "Operation was cancelled".to_string(),
                },
                &result,
            )?;
            updated.validate()?;
        }
        ControllerCommandVariant::Abort => {
            if current.status != crate::checkpoint::CheckpointStatus::ReconciliationRequired {
                return Err(CheckpointError::new(
                    "controller_command_invalid_state",
                    "abort requires reconciliation_required state",
                ));
            }
            let observation = current
                .model_call_journal
                .iter()
                .chain(current.tool_journal.iter())
                .find(|entry| entry.state == crate::checkpoint::OperationState::Ambiguous)
                .map(|entry| ResumeObservation {
                    operation_id: entry.operation_id.clone(),
                    operation_kind: entry.kind,
                    cycle_index: entry.cycle_index,
                    state: entry.state,
                    risk: "operator abort leaves the external operation outcome unknown"
                        .to_string(),
                    idempotency_support: entry.idempotency_support,
                });
            let mut result = control_result(
                &current,
                CompletionReason::Failed,
                "failed",
                Some("operator_abort_with_unknown_outcome"),
            );
            result.resume_observation = observation;
            updated.status = crate::checkpoint::CheckpointStatus::Failed;
            updated.active_host_interaction = None;
            updated.suspended_origin = None;
            updated.claim_token = None;
            updated.claimed_cycle = None;
            updated.lease_expires_at_ms = None;
            updated.terminal_result = Some(result.to_dict());
            updated.revision = current.revision + 1;
            append_control_event(
                &mut updated,
                &command.command_id,
                RunEventPayload::RunStateChanged {
                    state: "failed".to_string(),
                },
            )?;
            append_control_event_with_result(
                &mut updated,
                &command.command_id,
                RunEventPayload::RunFailed {
                    error: "failed".to_string(),
                },
                &result,
            )?;
            updated.validate()?;
        }
    }
    let outbox_state = if wake.action == "recovery_dispatch" {
        "pending"
    } else {
        "delivered"
    };
    let receipt = ControllerCommandReceipt {
        schema_version: crate::checkpoint::CONTROLLER_COMMAND_RECEIPT_SCHEMA.to_string(),
        command_id: command.command_id.clone(),
        command_digest: command.command_digest.clone(),
        handle: command.handle.clone(),
        resume_attempt: command.resume_attempt,
        expected_revision: command.expected_revision,
        resulting_revision: updated.revision,
        resulting_status: updated.status.as_str().to_string(),
        outbox_state: outbox_state.to_string(),
        outbox_action: wake.action.clone(),
        outbox_destination: wake.destination.clone(),
        outbox_attempt: 0,
    };
    receipt.validate()?;
    checkpoints.insert(current.checkpoint_key.clone(), updated);
    let resolution = ControllerCommandResolution::Applied {
        receipt: receipt.clone(),
        wake,
    };
    Ok((receipt, resolution))
}

/// Apply one command against a checkpoint plus its host record.  Redis uses
/// the same pure transition implementation as the memory and SQLite stores;
/// the enclosing store supplies the durable CAS and receipt transaction.
pub(crate) fn apply_controller_command_single(
    checkpoint: Checkpoint,
    host_record: Option<HostInteractionRecord>,
    command: &ControllerCommand,
) -> CheckpointResult<(
    Checkpoint,
    Option<HostInteractionRecord>,
    ControllerCommandReceipt,
    ControllerCommandResolution,
)> {
    let checkpoint_key = checkpoint.checkpoint_key.clone();
    let mut checkpoints = BTreeMap::new();
    checkpoints.insert(checkpoint_key.clone(), checkpoint);
    let mut ledger = ControllerLedger::default();
    if let Some(record) = host_record {
        ledger
            .host_interactions
            .insert(record.record_id.clone(), record);
    }
    let (receipt, resolution) = apply_controller_command(&mut checkpoints, &mut ledger, command)?;
    let updated = checkpoints.remove(&checkpoint_key).ok_or_else(|| {
        CheckpointError::new(
            "controller_command_internal",
            "transition dropped checkpoint",
        )
    })?;
    let updated_record = ledger.host_interactions.into_values().next();
    Ok((updated, updated_record, receipt, resolution))
}
