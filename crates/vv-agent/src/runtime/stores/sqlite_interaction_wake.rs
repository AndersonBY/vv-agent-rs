type SqliteControllerWakeRow = (
    ControllerCommandReceipt,
    String,
    String,
    u64,
    Option<String>,
    Option<u64>,
    Option<u64>,
    Option<String>,
);

fn load_controller_wake_row(
    transaction: &Transaction<'_>,
    command_id: &str,
    command_digest: &str,
) -> CheckpointResult<Option<SqliteControllerWakeRow>> {
    let raw = transaction
        .query_row(
            "SELECT receipt, command_digest, outbox_state, attempt, claim_token, lease_expires_at_ms, delivered_at_ms, last_error FROM controller_command_receipts WHERE command_id = ?1 AND command_digest = ?2",
            params![command_id, command_digest],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            },
        )
        .optional()
        .map_err(sqlite_error)?;
    raw.map(
        |(
            receipt,
            stored_digest,
            state,
            attempt,
            claim_token,
            lease,
            delivered_at,
            last_error,
        )| {
            if stored_digest != command_digest {
                return Err(CheckpointError::new(
                    "controller_command_conflict",
                    "controller wake digest does not match receipt",
                ));
            }
            let mut receipt = ControllerCommandReceipt::from_value(&serde_json::from_str(&receipt)?)?;
            if receipt.command_id != command_id || receipt.command_digest != stored_digest {
                return Err(CheckpointError::new(
                    "controller_command_conflict",
                    "controller wake receipt identity conflicts with its indexed row",
                ));
            }
            receipt.outbox_state = state;
            receipt.outbox_attempt = to_u64(attempt)?;
            receipt.validate()?;
            let lease = lease.map(to_u64).transpose()?;
            let delivered_at = delivered_at.map(to_u64).transpose()?;
            if (claim_token.is_some()) != lease.is_some()
                || (receipt.outbox_state == "claimed") != claim_token.is_some()
            {
                return Err(CheckpointError::new(
                    "controller_command_conflict",
                    "controller wake claim and lease are inconsistent",
                ));
            }
            if claim_token
                .as_ref()
                .is_some_and(|value| value.trim().is_empty() || value.len() > 512)
                || lease.is_some_and(|value| value > crate::checkpoint::MAX_WIRE_INTEGER)
                || delivered_at.is_some_and(|value| value > crate::checkpoint::MAX_WIRE_INTEGER)
                || last_error
                    .as_ref()
                    .is_some_and(|value| value.len() > crate::checkpoint::HOST_INTERACTION_CONTENT_MAX_UTF8_BYTES)
                || last_error
                    .as_ref()
                    .is_some_and(|value| crate::checkpoint::sanitize_host_text(value) != *value)
            {
                return Err(CheckpointError::new(
                    "controller_command_conflict",
                    "controller wake lifecycle metadata is invalid",
                ));
            }
            Ok((
                receipt,
                stored_digest,
                command_id.to_string(),
                to_u64(attempt)?,
                claim_token,
                lease,
                delivered_at,
                last_error,
            ))
        },
    )
    .transpose()
}

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
    let sanitized_error = last_error.map(crate::checkpoint::sanitize_host_text);
    if sanitized_error
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
                sanitized_error,
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
    let Some((mut receipt, _, _, _, _, lease, _, _)) =
        load_controller_wake_row(&transaction, command_id, command_digest)?
    else {
        transaction.commit().map_err(sqlite_error)?;
        return Ok(None);
    };
    let claimable = receipt.outbox_state == "pending"
        || (receipt.outbox_state == "claimed" && lease.is_some_and(|value| value <= now_ms));
    if !claimable || receipt.outbox_action != "recovery_dispatch" {
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
    if receipt.outbox_state != "claimed"
        || owner.as_deref() != Some(claim_token)
        || receipt.outbox_attempt != attempt
        || lease.is_none_or(|value| value == 0)
    {
        transaction.commit().map_err(sqlite_error)?;
        return Ok(None);
    }
    match outcome {
        "delivered" => {
            receipt.outbox_state = "delivered".to_string();
            update_controller_wake_row(
                &transaction,
                &receipt,
                None,
                None,
                Some(now_ms),
                None,
            )?;
        }
        "ambiguous" => {
            receipt.outbox_state = "ambiguous".to_string();
            update_controller_wake_row(
                &transaction,
                &receipt,
                None,
                None,
                None,
                error,
            )?;
        }
        _ => {
            return Err(CheckpointError::new(
                "controller_command_outbox_invalid",
                "wake completion outcome is invalid",
            ))
        }
    }
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

fn sqlite_reap_controller_command_wake(
    store: &SqliteCheckpointStore,
    command_id: &str,
    command_digest: &str,
    now_ms: u64,
) -> CheckpointResult<bool> {
    let mut connection = store.lock()?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    let Some((mut receipt, _, _, _, _, lease, _, _)) =
        load_controller_wake_row(&transaction, command_id, command_digest)?
    else {
        transaction.commit().map_err(sqlite_error)?;
        return Ok(false);
    };
    if receipt.outbox_state != "claimed" || lease.is_none_or(|value| value > now_ms) {
        transaction.commit().map_err(sqlite_error)?;
        return Ok(false);
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
    transaction.commit().map_err(sqlite_error)?;
    Ok(true)
}

fn load_controller_receipt(
    transaction: &Transaction<'_>,
    command_id: &str,
) -> CheckpointResult<Option<(ControllerCommandReceipt, ControllerCommand)>> {
    let raw = transaction
        .query_row(
            "SELECT receipt, command FROM controller_command_receipts WHERE command_id = ?1",
            params![command_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    raw.map(|(receipt, command)| {
        let receipt = ControllerCommandReceipt::from_value(&serde_json::from_str(&receipt)?)?;
        let command = ControllerCommand::from_value(&serde_json::from_str(&command)?)?;
        if receipt.command_id != command_id
            || receipt.command_id != command.command_id
            || receipt.command_digest != command.command_digest
        {
            return Err(CheckpointError::new(
                "controller_command_conflict",
                "controller receipt and command payload identity conflicts",
            ));
        }
        Ok((receipt, command))
    })
    .transpose()
}

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
    let (receipt, wake) = match sqlite_apply_controller_command(&transaction, &command) {
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
    if current.has_ambiguous_operation()
        && !matches!(&command.command, ControllerCommandVariant::Abort)
    {
        return Err(CheckpointError::new(
            "controller_command_ambiguity_requires_reconciliation",
            "controller command is blocked by an ambiguous operation",
        ));
    }
    if current.status == crate::checkpoint::CheckpointStatus::Deferred {
        return Err(CheckpointError::new(
            "controller_command_deferred_pending",
            "deferred resolution is an authoritative barrier",
        ));
    }
    if current.claim_token.is_some() {
        return Err(CheckpointError::new(
            "controller_command_claim_active",
            "controller command requires a released execution claim",
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
            sqlite_append_control_event(
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
            sqlite_append_control_event(
                &mut updated,
                &command.command_id,
                RunEventPayload::RunStateChanged {
                    state: resulting_state,
                },
            )?;
            updated.validate()?;
        }
        ControllerCommandVariant::Cancel => {
            let mut result = sqlite_control_result(
                &current,
                CompletionReason::Cancelled,
                "Operation was cancelled",
                Some("cancelled"),
            );
            result.completion_reason = Some(CompletionReason::Cancelled);
            updated.status = crate::checkpoint::CheckpointStatus::Failed;
            updated.active_host_interaction = None;
            updated.suspended_origin = None;
            updated.claim_token = None;
            updated.claimed_cycle = None;
            updated.lease_expires_at_ms = None;
            updated.terminal_result = Some(result.to_dict());
            updated.revision = current.revision + 1;
            sqlite_append_control_event(
                &mut updated,
                &command.command_id,
                RunEventPayload::RunStateChanged {
                    state: "failed".to_string(),
                },
            )?;
            sqlite_append_control_event_with_result(
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
            let mut result = sqlite_control_result(
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
            sqlite_append_control_event(
                &mut updated,
                &command.command_id,
                RunEventPayload::RunStateChanged {
                    state: "failed".to_string(),
                },
            )?;
            sqlite_append_control_event_with_result(
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
    if !update_row(transaction, &values, Some(current.revision), None)? {
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
