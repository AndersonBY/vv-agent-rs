
fn update_controller_wake_row(
    transaction: &Transaction<'_>,
    receipt: &ControllerCommandReceipt,
    claim_token: Option<&str>,
    lease_expires_at_ms: Option<u64>,
    delivered_at_ms: Option<u64>,
    last_error: Option<&str>,
) -> CheckpointResult<()> {
    receipt.validate()?;
    if claim_token
        .is_some_and(|value| value.trim().is_empty() || value.len() > 512)
        || lease_expires_at_ms.is_some_and(|value| value > crate::checkpoint::MAX_WIRE_INTEGER)
        || delivered_at_ms.is_some_and(|value| value > crate::checkpoint::MAX_WIRE_INTEGER)
        || (receipt.outbox_state == "claimed") != claim_token.is_some()
    {
        return Err(CheckpointError::new(
            "controller_command_outbox_invalid",
            "wake claim or lifecycle timestamp is invalid",
        ));
    }
    if last_error
        .as_ref()
        .is_some_and(|value| value.len() > crate::checkpoint::HOST_INTERACTION_CONTENT_MAX_UTF8_BYTES)
    {
        return Err(CheckpointError::new(
            "controller_command_outbox_invalid",
            "wake last_error is too large",
        ));
    }
    let wire = serde_json::to_string(&receipt.to_value())?;
    transaction
        .execute(
            "UPDATE controller_command_receipts SET receipt = ?1, outbox_state = ?2, attempt = ?3, claim_token = ?4, lease_expires_at_ms = ?5, delivered_at_ms = ?6, last_error = ?7 WHERE command_id = ?8 AND command_digest = ?9",
            params![
                wire,
                receipt.outbox_state,
                to_i64(receipt.outbox_attempt, "outbox_attempt")?,
                claim_token,
                lease_expires_at_ms
                    .map(|value| to_i64(value, "lease_expires_at_ms"))
                    .transpose()?,
                delivered_at_ms
                    .map(|value| to_i64(value, "delivered_at_ms"))
                    .transpose()?,
                last_error,
                receipt.command_id,
                receipt.command_digest,
            ],
        )
        .map_err(sqlite_error)?;
    Ok(())
}

fn sqlite_claim_controller_command_wake(
    store: &SqliteCheckpointStore,
    command_id: &str,
    command_digest: &str,
    claim_token: &str,
    lease_expires_at_ms: u64,
    now_ms: u64,
) -> CheckpointResult<Option<ControllerCommandReceipt>> {
    if claim_token.trim().is_empty() || lease_expires_at_ms <= now_ms {
        return Err(CheckpointError::new(
            "controller_command_outbox_invalid",
            "wake claim token must be non-empty and lease must be in the future",
        ));
    }
    let mut connection = store.lock()?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    let Some((mut receipt, _, _, _, owner, lease, _, _)) =
        load_controller_wake_row(&transaction, command_id, command_digest)?
    else {
        transaction.commit().map_err(sqlite_error)?;
        return Ok(None);
    };
    if receipt.outbox_state == "claimed"
        && owner.as_deref() == Some(claim_token)
        && lease.is_some_and(|value| value > now_ms)
    {
        transaction.commit().map_err(sqlite_error)?;
        return Ok(Some(receipt));
    }
    if receipt.outbox_state == "claimed" && lease.is_some_and(|value| value > now_ms) {
        transaction.commit().map_err(sqlite_error)?;
        return Err(CheckpointError::new(
            "controller_command_outbox_stale",
            "wake is claimed by another owner",
        ));
    }
    let claimable = receipt.outbox_state == "pending"
        || (receipt.outbox_state == "claimed" && lease.is_some_and(|value| value <= now_ms));
    if !claimable || receipt.outbox_action != "recovery_dispatch" {
        if receipt.outbox_action == "none" || receipt.outbox_state == "delivered" {
            transaction.commit().map_err(sqlite_error)?;
            return Ok(Some(receipt));
        }
        if receipt.outbox_state == "ambiguous" {
            return Err(CheckpointError::new(
                "controller_command_stale",
                "controller wake requires reconciliation",
            ));
        }
        transaction.commit().map_err(sqlite_error)?;
        return Ok(None);
    }
    receipt.outbox_state = "claimed".to_string();
    receipt.outbox_attempt = receipt.outbox_attempt.saturating_add(1);
    update_controller_wake_row(
        &transaction,
        &receipt,
        Some(claim_token),
        Some(lease_expires_at_ms),
        None,
        None,
    )?;
    transaction.commit().map_err(sqlite_error)?;
    Ok(Some(receipt))
}

#[allow(clippy::too_many_arguments)]
fn sqlite_complete_controller_command_wake(
    store: &SqliteCheckpointStore,
    command_id: &str,
    command_digest: &str,
    claim_token: &str,
    attempt: u64,
    outcome: &str,
    now_ms: u64,
    error: Option<&str>,
) -> CheckpointResult<Option<ControllerCommandReceipt>> {
    if !matches!(outcome, "delivered" | "ambiguous") {
        return Err(CheckpointError::new(
            "controller_command_outbox_invalid",
            "wake completion outcome is invalid",
        ));
    }
    let mut connection = store.lock()?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    let Some((mut receipt, _, _, _, owner, lease, _, _)) =
        load_controller_wake_row(&transaction, command_id, command_digest)?
    else {
        transaction.commit().map_err(sqlite_error)?;
        return Ok(None);
    };
    if matches!(receipt.outbox_state.as_str(), "delivered" | "ambiguous") {
        if receipt.outbox_state != outcome {
            return Err(CheckpointError::new(
                "controller_command_stale",
                "controller wake has already completed",
            ));
        }
        transaction.commit().map_err(sqlite_error)?;
        return Ok(Some(receipt));
    }
    if receipt.outbox_state != "claimed"
        || owner.as_deref() != Some(claim_token)
        || receipt.outbox_attempt != attempt
        || lease.is_none_or(|value| value == 0)
    {
        transaction.commit().map_err(sqlite_error)?;
        return Ok(None);
    }
    receipt.outbox_state = outcome.to_string();
    update_controller_wake_row(
        &transaction,
        &receipt,
        None,
        None,
        (outcome == "delivered").then_some(now_ms),
        if outcome == "ambiguous" { error } else { None },
    )?;
    transaction.commit().map_err(sqlite_error)?;
    Ok(Some(receipt))
}

fn sqlite_reconcile_controller_command_wake(
    store: &SqliteCheckpointStore,
    command_id: &str,
    command_digest: &str,
    outcome: &str,
    now_ms: u64,
) -> CheckpointResult<Option<ControllerCommandReceipt>> {
    let mut connection = store.lock()?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    let Some((mut receipt, _, _, _, _, _, _, _)) =
        load_controller_wake_row(&transaction, command_id, command_digest)?
    else {
        transaction.commit().map_err(sqlite_error)?;
        return Ok(None);
    };
    if receipt.outbox_state != "ambiguous" {
        transaction.commit().map_err(sqlite_error)?;
        return Ok(None);
    }
    match outcome {
        "delivered" => {
            receipt.outbox_state = "delivered".to_string();
            update_controller_wake_row(&transaction, &receipt, None, None, Some(now_ms), None)?;
        }
        "retry" => {
            receipt.outbox_state = "pending".to_string();
            update_controller_wake_row(&transaction, &receipt, None, None, None, None)?;
        }
        _ => {
            return Err(CheckpointError::new(
                "controller_command_outbox_invalid",
                "wake reconciliation outcome is invalid",
            ))
        }
    }
    transaction.commit().map_err(sqlite_error)?;
    Ok(Some(receipt))
}

fn sqlite_reap_controller_command_wakes(
    store: &SqliteCheckpointStore,
    checkpoint_key: &str,
    now_ms: u64,
) -> CheckpointResult<Vec<crate::checkpoint::ControllerCommandWakeRecord>> {
    let mut connection = store.lock()?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    let now_ms_i64 = to_i64(now_ms, "now_ms")?;
    let candidates = {
        let mut statement = transaction
            .prepare(
                "SELECT command_id, command_digest FROM controller_command_receipts \
                 WHERE checkpoint_key = ?1 AND outbox_action = 'recovery_dispatch' \
                   AND (outbox_state = 'pending' OR \
                        (outbox_state = 'claimed' AND lease_expires_at_ms IS NOT NULL \
                         AND lease_expires_at_ms <= ?2)) \
                 ORDER BY expected_revision ASC, command_id ASC",
            )
            .map_err(sqlite_error)?;
        let rows = statement
            .query_map(params![checkpoint_key, now_ms_i64], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(sqlite_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_error)?;
        rows
    };
    let mut reaped = Vec::with_capacity(candidates.len());
    for (command_id, command_digest) in candidates {
        let Some((mut receipt, _, _, attempt, claim_token, lease, delivered_at, last_error)) =
            load_controller_wake_row(&transaction, &command_id, &command_digest)?
        else {
            continue;
        };
        if receipt.handle.checkpoint_key != checkpoint_key
            || receipt.outbox_action != "recovery_dispatch"
        {
            continue;
        }
        if receipt.outbox_state == "pending" {
            reaped.push(crate::checkpoint::ControllerCommandWakeRecord::from_receipt_lifecycle(
                &receipt,
                crate::checkpoint::controller_receipt_outbox_id(
                    &receipt.command_id,
                    &receipt.command_digest,
                )?,
                attempt,
                claim_token,
                lease,
                delivered_at,
                last_error,
            )?);
            continue;
        }
        if receipt.outbox_state != "claimed"
            || lease.is_none_or(|value| value > now_ms)
        {
            continue;
        }
        receipt.outbox_state = "pending".to_string();
        update_controller_wake_row(
            &transaction,
            &receipt,
            None,
            None,
            None,
            Some("controller_wake_claim_expired"),
        )?;
        reaped.push(crate::checkpoint::ControllerCommandWakeRecord::from_receipt_lifecycle(
            &receipt,
            crate::checkpoint::controller_receipt_outbox_id(
                &receipt.command_id,
                &receipt.command_digest,
            )?,
            attempt,
            None,
            None,
            None,
            Some("controller_wake_claim_expired".to_string()),
        )?);
    }
    transaction.commit().map_err(sqlite_error)?;
    Ok(reaped)
}

fn sqlite_find_resolved_pending_host_interaction(
    store: &SqliteCheckpointStore,
    checkpoint_key: &str,
) -> CheckpointResult<Option<HostInteractionRecord>> {
    let mut connection = store.lock()?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Deferred)
        .map_err(sqlite_error)?;
    let interaction_ids = {
        let mut statement = transaction
            .prepare(
                "SELECT interaction_id FROM host_interaction_records \
                 WHERE checkpoint_key = ?1 AND state = 'resolved_pending' \
                 ORDER BY record_id ASC LIMIT 2",
            )
            .map_err(sqlite_error)?;
        let rows = statement
            .query_map(params![checkpoint_key], |row| row.get::<_, String>(0))
            .map_err(sqlite_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_error)?;
        rows
    };
    if interaction_ids.len() > 1 {
        return Err(CheckpointError::new(
            "host_interaction_recovery_stale",
            "checkpoint has multiple resolved host interaction records",
        ));
    }
    let record = interaction_ids
        .first()
        .map(|interaction_id| load_host_record_by_interaction(&transaction, checkpoint_key, interaction_id))
        .transpose()?
        .flatten();
    transaction.commit().map_err(sqlite_error)?;
    Ok(record)
}

include!("sqlite_interaction_receipt.rs");

fn insert_controller_receipt(
    transaction: &Transaction<'_>,
    command: &ControllerCommand,
    receipt: &ControllerCommandReceipt,
) -> CheckpointResult<()> {
    let handle = serde_json::to_string(&command.handle.to_value())?;
    let command_wire = serde_json::to_string(&command.to_value())?;
    let receipt_wire = serde_json::to_string(&receipt.to_value())?;
    let outbox_id = crate::checkpoint::controller_receipt_outbox_id(
        &command.command_id,
        &command.command_digest,
    )?;
    transaction
        .execute(
            "INSERT INTO controller_command_receipts (command_id, checkpoint_key, handle, command_digest, command, resume_attempt, expected_revision, receipt, resulting_status, resulting_revision, outbox_state, outbox_id, outbox_action, outbox_destination, attempt, claim_token, lease_expires_at_ms, delivered_at_ms, last_error) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, 0, NULL, NULL, NULL, NULL)",
            params![
                command.command_id,
                command.handle.checkpoint_key,
                handle,
                command.command_digest,
                command_wire,
                to_i64(command.resume_attempt, "resume_attempt")?,
                to_i64(command.expected_revision, "expected_revision")?,
                receipt_wire,
                receipt.resulting_status,
                to_i64(receipt.resulting_revision, "resulting_revision")?,
                receipt.outbox_state,
                outbox_id,
                receipt.outbox_action,
                receipt.outbox_destination,
            ],
        )
        .map_err(sqlite_error)?;
    Ok(())
}

fn sqlite_resolve_controller_command(
    store: &SqliteCheckpointStore,
    command: ControllerCommand,
) -> CheckpointResult<ControllerCommandResolution> {
    command.validate()?;
    let mut connection = store.lock()?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    if let Some((receipt, existing)) = load_controller_receipt(&transaction, &command.command_id)? {
        if existing.command_digest != command.command_digest {
            return Err(CheckpointError::new(
                "controller_command_conflict",
                "command_id is already bound to a different command digest",
            ));
        }
        let Some(checkpoint) =
            load_row_transaction(&transaction, &command.handle.checkpoint_key)?
        else {
            transaction.commit().map_err(sqlite_error)?;
            return Ok(ControllerCommandResolution::Rejected {
                error: CheckpointError::new(
                    "controller_command_stale",
                    "checkpoint does not exist",
                )
                .to_string(),
            });
        };
        let wake = sqlite_command_wake(&existing, &receipt, &checkpoint);
        transaction.commit().map_err(sqlite_error)?;
        return Ok(ControllerCommandResolution::Replayed { receipt, wake });
    }
    let (receipt, wake) = match sqlite_apply_controller_command(
        &transaction,
        &command,
        sqlite_current_time_ms(),
    ) {
        Ok(result) => result,
        Err(error)
            if matches!(
                error.code(),
                "controller_command_stale" | "controller_command_terminal"
            ) => {
                transaction.commit().map_err(sqlite_error)?;
                return Ok(ControllerCommandResolution::Rejected {
                    error: error.to_string(),
                });
            }
        Err(error) => return Err(error),
    };
    insert_controller_receipt(&transaction, &command, &receipt)?;
    transaction.commit().map_err(sqlite_error)?;
    Ok(ControllerCommandResolution::Applied { receipt, wake })
}

fn sqlite_admit_controller_command(
    store: &SqliteCheckpointStore,
    command: ControllerCommand,
) -> CheckpointResult<ControllerCommandReceipt> {
    match sqlite_resolve_controller_command(store, command)? {
        ControllerCommandResolution::Applied { receipt, .. }
        | ControllerCommandResolution::Replayed { receipt, .. } => Ok(receipt),
        ControllerCommandResolution::Rejected { error } => Err(CheckpointError::new(
            "controller_command_invalid_state",
            error,
        )),
    }
}

fn sqlite_get_controller_command_receipt(
    store: &SqliteCheckpointStore,
    command_id: &str,
) -> CheckpointResult<Option<ControllerCommandReceipt>> {
    let mut connection = store.lock()?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    let result = load_controller_receipt(&transaction, command_id)?.map(|(receipt, _)| receipt);
    transaction.commit().map_err(sqlite_error)?;
    Ok(result)
}

fn sqlite_get_controller_command(
    store: &SqliteCheckpointStore,
    command_id: &str,
) -> CheckpointResult<Option<ControllerCommand>> {
    let mut connection = store.lock()?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    let result = load_controller_receipt(&transaction, command_id)?.map(|(_, command)| command);
    transaction.commit().map_err(sqlite_error)?;
    Ok(result)
}

fn sqlite_command_wake(
    command: &ControllerCommand,
    receipt: &ControllerCommandReceipt,
    checkpoint: &Checkpoint,
) -> ControllerCommandWake {
    if receipt.outbox_action != "recovery_dispatch" {
        return ControllerCommandWake::none();
    }
    let logical_cycle = match &command.command {
        ControllerCommandVariant::HostInteractionResponse { logical_cycle, .. } => *logical_cycle,
        _ => checkpoint.cycle_index.saturating_add(1),
    };
    ControllerCommandWake::recovery(logical_cycle)
}

fn sqlite_apply_controller_command(
    transaction: &Transaction<'_>,
    command: &ControllerCommand,
    now_ms: u64,
) -> CheckpointResult<(ControllerCommandReceipt, ControllerCommandWake)> {
    let current =
        load_row_transaction(transaction, &command.handle.checkpoint_key)?.ok_or_else(|| {
            CheckpointError::new("controller_command_stale", "checkpoint does not exist")
        })?;
    if current.root_run_id != command.handle.run_id || current.trace_id != command.handle.trace_id {
        return Err(CheckpointError::new(
            "controller_command_stale",
            "controller handle does not match checkpoint",
        ));
    }
    if current.resume_attempt != command.resume_attempt
        || current.revision != command.expected_revision
    {
        return Err(CheckpointError::new(
            "controller_command_stale",
            "controller fence does not match checkpoint",
        ));
    }
    if current.terminal_result.is_some() || current.status.is_terminal() {
        return Err(CheckpointError::new(
            "controller_command_terminal",
            "controller commands cannot mutate a terminal checkpoint",
        ));
    }
    let claim_expired = current.claim_token.is_some()
        && current
            .lease_expires_at_ms
            .is_some_and(|lease| lease <= now_ms);
    if current.has_ambiguous_operation()
        && !matches!(&command.command, ControllerCommandVariant::Abort)
        && !(matches!(&command.command, ControllerCommandVariant::Cancel)
            && current.claim_token.is_some())
    {
        return Err(CheckpointError::new(
            "controller_command_ambiguity_requires_reconciliation",
            "controller command is blocked by an ambiguous operation",
        ));
    }
    if (current.status == crate::checkpoint::CheckpointStatus::Deferred
        || current.tool_journal.iter().any(|entry| entry.state == crate::checkpoint::OperationState::Deferred))
        && !matches!(&command.command, ControllerCommandVariant::Suspend | ControllerCommandVariant::Resume | ControllerCommandVariant::Cancel)
    {
        return Err(CheckpointError::new(
            "controller_command_deferred_pending",
            "deferred resolution is an authoritative barrier",
        ));
    }
    if current.claim_token.is_some()
        && !claim_expired
        && !matches!(&command.command, ControllerCommandVariant::Cancel)
    {
        return Err(CheckpointError::new(
            "controller_command_claim_active",
            "controller command requires a released execution claim",
        ));
    }
    let mut updated = current.clone();
    if claim_expired
        && matches!(
            &command.command,
            ControllerCommandVariant::Cancel | ControllerCommandVariant::Suspend
        )
    {
        if matches!(&command.command, ControllerCommandVariant::Cancel) {
            updated.resume_attempt = updated.resume_attempt.checked_add(1).ok_or_else(|| {
                CheckpointError::new(
                    "checkpoint_resume_attempt_invalid",
                    "resume_attempt overflow",
                )
            })?;
            updated.cancel_requested = true;
        }
        updated.claim_token = None;
        updated.claimed_cycle = None;
        updated.lease_expires_at_ms = None;
    }
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
            let mut record = load_host_record_by_interaction(
                transaction,
                &current.checkpoint_key,
                interaction_id,
            )?
                .ok_or_else(|| {
                    CheckpointError::new(
                        "controller_command_stale",
                        "host interaction record does not exist",
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
            update_host_record(transaction, &record)?;
        }
        ControllerCommandVariant::Suspend => {
            let origin = match current.status {
                crate::checkpoint::CheckpointStatus::Running => SuspendedOrigin::running(),
                crate::checkpoint::CheckpointStatus::Deferred => SuspendedOrigin {
                    status: "deferred".to_string(),
                    active_host_interaction: None,
                },
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
                "deferred" if current.tool_journal.iter().any(|entry| entry.state == crate::checkpoint::OperationState::Deferred) => {
                    updated.status = crate::checkpoint::CheckpointStatus::Deferred;
                    updated.suspended_origin = None;
                }
                "running" | "deferred" => {
                    updated.status = crate::checkpoint::CheckpointStatus::Running;
                    updated.suspended_origin = None;
                    wake = ControllerCommandWake::recovery(current.cycle_index + 1);
                }
                "host_interaction" => {
                    let request = origin.active_host_interaction.clone().ok_or_else(|| {
                        CheckpointError::new(
                            "controller_command_invalid_state",
                            "host origin has no request",
                        )
                    })?;
                    let record = load_host_record_by_interaction(
                        transaction,
                        &current.checkpoint_key,
                        &request.interaction_id,
                    )?;
                    if record
                        .as_ref()
                        .is_some_and(|record| record.state == "resolved_pending")
                    {
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
            if current.claim_token.is_some() && !claim_expired {
                if !current.cancel_requested {
                    updated.cancel_requested = true;
                    append_cancel_requested_event(&mut updated, &command.command_id)?;
                }
                updated.validate()?;
            } else {
                let mut result = control_result(
                    &current,
                    CompletionReason::Cancelled,
                    "Operation was cancelled",
                    Some("cancelled_with_unknown_outcome"),
                );
                result.completion_reason = Some(CompletionReason::Cancelled);
                updated.status = crate::checkpoint::CheckpointStatus::Failed;
                updated.active_host_interaction = None;
                updated.suspended_origin = None;
                updated.claim_token = None;
                updated.claimed_cycle = None;
                updated.lease_expires_at_ms = None;
                updated.terminal_result = Some(result.to_dict());
                close_unresolved_tools(&mut updated, "cancelled")?;
                updated.model_call_journal.clear();
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
                "Operator accepted that the external outcome is unknown.",
                Some("operator_abort_with_unknown_outcome"),
            );
            result.resume_observations = observation.into_iter().collect();
            updated.status = crate::checkpoint::CheckpointStatus::Failed;
            updated.active_host_interaction = None;
            updated.suspended_origin = None;
            updated.claim_token = None;
            updated.claimed_cycle = None;
            updated.lease_expires_at_ms = None;
            updated.terminal_result = Some(result.to_dict());
            close_unresolved_tools(&mut updated, "operator_abort")?;
            updated.model_call_journal.clear();
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
    let values = SqlValues::from_checkpoint(&updated)?;
    let claim_fence = current.claim_token.as_deref();
    if !update_row(transaction, &values, Some(current.revision), claim_fence)? {
        return Err(CheckpointError::new(
            "checkpoint_revision_conflict",
            "controller command lost the checkpoint CAS",
        ));
    }
    let receipt = ControllerCommandReceipt {
        schema_version: crate::checkpoint::CONTROLLER_COMMAND_RECEIPT_SCHEMA.to_string(),
        command_id: command.command_id.clone(),
        command_digest: command.command_digest.clone(),
        handle: command.handle.clone(),
        resume_attempt: command.resume_attempt,
        expected_revision: command.expected_revision,
        resulting_revision: updated.revision,
        resulting_status: updated.status.as_str().to_string(),
        outbox_state: if wake.action == "recovery_dispatch" {
            "pending".to_string()
        } else {
            "delivered".to_string()
        },
        outbox_action: wake.action.clone(),
        outbox_destination: wake.destination.clone(),
        outbox_attempt: 0,
    };
    receipt.validate()?;
    Ok((receipt, wake))
}
