fn sqlite_produce_host_interaction(
    store: &SqliteCheckpointStore,
    request: HostInteractionRequest,
    context: &crate::checkpoint::HostInteractionAdmissionContext,
) -> CheckpointResult<HostInteractionOutcome> {
    request.validate()?;
    context.validate()?;
    let context_is_live = context.validate_live_lease().is_ok();
    let mut connection = store.lock()?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    if let Some(existing) = load_host_record_by_interaction(
        &transaction,
        &context.checkpoint_key,
        &request.interaction_id,
    )? {
        if existing.request != request {
            return Err(CheckpointError::new(
                "host_interaction_conflict",
                "interaction identity is already bound to a different request",
            ));
        }
        let checkpoint =
            load_row_transaction(&transaction, &existing.checkpoint_key)?.ok_or_else(|| {
                CheckpointError::new(
                    "host_interaction_conflict",
                    "host interaction checkpoint is missing",
                )
            })?;
        let notification_id = notification_id_for(&existing.record_id);
        let notification = load_notification(&transaction, &notification_id)?.ok_or_else(|| {
            CheckpointError::new(
                "host_interaction_conflict",
                "host interaction notification is missing",
            )
        })?;
        let outcome = sqlite_host_interaction_outcome(
            &request,
            checkpoint.revision,
            "replayed",
            &existing.record_id,
            &notification,
        )?;
        transaction.commit().map_err(sqlite_error)?;
        return Ok(outcome);
    }
    let checkpoint_key = context.checkpoint_key.clone();
    let current = load_row_transaction(&transaction, &checkpoint_key)?.ok_or_else(|| {
        CheckpointError::new(
            "host_interaction_claim_required",
            "checkpoint disappeared during host interaction admission",
        )
    })?;
    if current.status != crate::checkpoint::CheckpointStatus::Running
        || current.revision != context.expected_revision
        || current.claim_token.as_deref() != Some(context.claim_token.as_str())
        || current.claimed_cycle != Some(context.claimed_cycle)
        || request.logical_cycle != context.claimed_cycle
        || current.lease_expires_at_ms != Some(context.lease_expires_at_ms)
        || !context_is_live
    {
        return Err(CheckpointError::new(
            "host_interaction_claim_required",
            "host interaction admission claim is stale or expired",
        ));
    }
    let claim_token = context.claim_token.as_str();
    let record_id = record_id_for(&checkpoint_key, &request);
    let notification_id = notification_id_for(&record_id);
    let notification_payload = HostInteractionNotificationPayload {
        schema_version: HOST_INTERACTION_NOTIFICATION_SCHEMA.to_string(),
        notification_id: notification_id.clone(),
        record_id: record_id.clone(),
        interaction_id: request.interaction_id.clone(),
        logical_cycle: request.logical_cycle,
        status: "host_interaction".to_string(),
        wait_reason: "host_interaction".to_string(),
        prompt: sqlite_sanitize_public_prompt(&request.prompt),
    };
    notification_payload.validate()?;
    let notification = HostInteractionNotificationRecord {
        notification_id: notification_id.clone(),
        checkpoint_key: checkpoint_key.clone(),
        record_id: record_id.clone(),
        payload: notification_payload.clone(),
        payload_digest: notification_payload.digest()?,
        outbox_state: NotificationOutboxState::Pending,
        claim_token: None,
        lease_expires_at_ms: None,
        attempt: 0,
        delivered_at_ms: None,
        aborted_at_ms: None,
        abort_reason: None,
        last_error: None,
    };
    notification.validate()?;
    let record = HostInteractionRecord {
        schema_version: HOST_INTERACTION_RECORD_SCHEMA.to_string(),
        record_id: record_id.clone(),
        checkpoint_key: checkpoint_key.clone(),
        interaction_id: request.interaction_id.clone(),
        logical_cycle: request.logical_cycle,
        attempt: 0,
        claim_token: None,
        lease_expires_at_ms: None,
        request: request.clone(),
        request_digest: request.request_digest.clone(),
        state: "active".to_string(),
        response: None,
        response_digest: None,
        command_id: None,
        resolved_revision: None,
        consumed_revision: None,
        last_error: None,
    };
    record.validate()?;
    let cycle_index = u32::try_from(request.logical_cycle).map_err(|_| {
        CheckpointError::new(
            "host_interaction_cycle_invalid",
            "logical cycle does not fit RunEvent",
        )
    })?;
    let mut event = RunEvent::new(
        current.root_run_id.clone(),
        current.trace_id.clone(),
        "vv-agent",
        Some(cycle_index.saturating_sub(1)),
        RunEventPayload::HostInteractionRequested {
            checkpoint_key: current.checkpoint_key.clone(),
            resume_attempt: current.resume_attempt,
            interaction_id: request.interaction_id.clone(),
            logical_cycle: request.logical_cycle,
            operation_id: request.operation_id.clone(),
            tool_call_id: request.tool_call_id.clone(),
            request_digest: request.request_digest.clone(),
            prompt: notification_payload.prompt.clone(),
        },
    );
    event.event_id = EventId::stable(format!("host-interaction-requested-{record_id}"))
        .map_err(|error| CheckpointError::new("event_identity_conflict", error))?;
    let mut updated = current.clone();
    updated.status = crate::checkpoint::CheckpointStatus::HostInteraction;
    updated.active_host_interaction = Some(request.clone());
    updated.claim_token = None;
    updated.claimed_cycle = None;
    updated.lease_expires_at_ms = None;
    updated.revision = current
        .revision
        .checked_add(1)
        .ok_or_else(|| CheckpointError::new("checkpoint_revision_overflow", "revision overflow"))?;
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
    if !update_row(
        &transaction,
        &values,
        Some(current.revision),
        Some(claim_token),
    )? {
        return Err(CheckpointError::new(
            "checkpoint_revision_conflict",
            "host interaction claim was lost",
        ));
    }
    insert_host_record(&transaction, &record)?;
    insert_notification(&transaction, &notification)?;
    let outcome = sqlite_host_interaction_outcome(
        &request,
        updated.revision,
        "admitted",
        &record_id,
        &notification,
    )?;
    transaction.commit().map_err(sqlite_error)?;
    Ok(outcome)
}

fn sqlite_host_interaction_outcome(
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

fn sqlite_sanitize_public_prompt(prompt: &str) -> String {
    crate::checkpoint::sanitize_public_text(prompt)
}

fn insert_host_record(
    transaction: &Transaction<'_>,
    record: &HostInteractionRecord,
) -> CheckpointResult<()> {
    transaction
        .execute(
            "INSERT INTO host_interaction_records (record_id, checkpoint_key, interaction_id, logical_cycle, request, request_digest, state, attempt, claim_token, lease_expires_at_ms, response, response_digest, command_id, resolved_revision, consumed_revision, last_error) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                record.record_id,
                record.checkpoint_key,
                record.interaction_id,
                to_i64(record.logical_cycle, "logical_cycle")?,
                serde_json::to_string(&record.request.to_value())?,
                record.request_digest,
                record.state,
                to_i64(record.attempt, "attempt")?,
                record.claim_token,
                record.lease_expires_at_ms.map(|value| to_i64(value, "lease_expires_at_ms")).transpose()?,
                Option::<String>::None,
                record.response_digest,
                record.command_id,
                record.resolved_revision.map(|value| to_i64(value, "resolved_revision")).transpose()?,
                record.consumed_revision.map(|value| to_i64(value, "consumed_revision")).transpose()?,
                record.last_error,
            ],
        )
        .map_err(sqlite_error)?;
    Ok(())
}

fn insert_notification(
    transaction: &Transaction<'_>,
    notification: &HostInteractionNotificationRecord,
) -> CheckpointResult<()> {
    transaction
        .execute(
            "INSERT INTO host_interaction_notification_outbox (notification_id, checkpoint_key, record_id, payload, payload_digest, outbox_state, claim_token, lease_expires_at_ms, attempt, delivered_at_ms, aborted_at_ms, abort_reason, last_error) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                notification.notification_id,
                notification.checkpoint_key,
                notification.record_id,
                serde_json::to_string(&notification.payload.to_value())?,
                notification.payload_digest,
                notification.outbox_state.as_str(),
                notification.claim_token,
                notification.lease_expires_at_ms.map(|value| to_i64(value, "lease_expires_at_ms")).transpose()?,
                to_i64(notification.attempt, "attempt")?,
                notification.delivered_at_ms.map(|value| to_i64(value, "delivered_at_ms")).transpose()?,
                notification.aborted_at_ms.map(|value| to_i64(value, "aborted_at_ms")).transpose()?,
                notification.abort_reason,
                notification.last_error,
            ],
        )
        .map_err(sqlite_error)?;
    Ok(())
}

fn load_host_record_by_interaction(
    transaction: &Transaction<'_>,
    checkpoint_key: &str,
    interaction_id: &str,
) -> CheckpointResult<Option<HostInteractionRecord>> {
    let mut statement = transaction
        .prepare("SELECT record_id, checkpoint_key, interaction_id, logical_cycle, request, request_digest, state, attempt, claim_token, lease_expires_at_ms, response, response_digest, command_id, resolved_revision, consumed_revision, last_error FROM host_interaction_records WHERE checkpoint_key = ?1 AND interaction_id = ?2")
        .map_err(sqlite_error)?;
    let raw = statement
        .query_row(params![checkpoint_key, interaction_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<i64>>(9)?,
                row.get::<_, Option<String>>(10)?,
                row.get::<_, Option<String>>(11)?,
                row.get::<_, Option<String>>(12)?,
                row.get::<_, Option<i64>>(13)?,
                row.get::<_, Option<i64>>(14)?,
                row.get::<_, Option<String>>(15)?,
            ))
        })
        .optional()
        .map_err(sqlite_error)?;
    raw.map(
        |(
            record_id,
            checkpoint_key,
            interaction_id,
            logical_cycle,
            request,
            request_digest,
            state,
            attempt,
            claim_token,
            lease_expires_at_ms,
            response,
            response_digest,
            command_id,
            resolved_revision,
            consumed_revision,
            last_error,
        )| {
            let request = serde_json::from_str::<Value>(&request)?;
            let response = response
                .as_deref()
                .map(serde_json::from_str::<Value>)
                .transpose()?;
            let record = HostInteractionRecord {
                schema_version: HOST_INTERACTION_RECORD_SCHEMA.to_string(),
                record_id,
                checkpoint_key,
                interaction_id,
                logical_cycle: to_u64(logical_cycle)?,
                attempt: to_u64(attempt)?,
                claim_token,
                lease_expires_at_ms: lease_expires_at_ms.map(to_u64).transpose()?,
                request: HostInteractionRequest::from_value(&request)?,
                request_digest,
                state,
                response: response
                    .as_ref()
                    .map(HostInteractionResponse::from_value)
                    .transpose()?,
                response_digest,
                command_id,
                resolved_revision: resolved_revision.map(to_u64).transpose()?,
                consumed_revision: consumed_revision.map(to_u64).transpose()?,
                last_error,
            };
            record.validate()?;
            Ok(record)
        },
    )
    .transpose()
}

fn load_notification(
    transaction: &Transaction<'_>,
    notification_id: &str,
) -> CheckpointResult<Option<HostInteractionNotificationRecord>> {
    let mut statement = transaction
        .prepare("SELECT notification_id, checkpoint_key, record_id, payload, payload_digest, outbox_state, claim_token, lease_expires_at_ms, attempt, delivered_at_ms, aborted_at_ms, abort_reason, last_error FROM host_interaction_notification_outbox WHERE notification_id = ?1")
        .map_err(sqlite_error)?;
    let raw = statement
        .query_row(params![notification_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<i64>>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, Option<i64>>(9)?,
                row.get::<_, Option<i64>>(10)?,
                row.get::<_, Option<String>>(11)?,
                row.get::<_, Option<String>>(12)?,
            ))
        })
        .optional()
        .map_err(sqlite_error)?;
    raw.map(
        |(
            notification_id,
            checkpoint_key,
            record_id,
            payload,
            payload_digest,
            outbox_state,
            claim_token,
            lease_expires_at_ms,
            attempt,
            delivered_at_ms,
            aborted_at_ms,
            abort_reason,
            last_error,
        )| {
            let payload =
                HostInteractionNotificationPayload::from_value(&serde_json::from_str(&payload)?)?;
            let outbox_state = match outbox_state.as_str() {
                "pending" => NotificationOutboxState::Pending,
                "claimed" => NotificationOutboxState::Claimed,
                "delivered" => NotificationOutboxState::Delivered,
                "ambiguous" => NotificationOutboxState::Ambiguous,
                "aborted" => NotificationOutboxState::Aborted,
                _ => {
                    return Err(CheckpointError::new(
                        "notification_conflict",
                        "unknown notification outbox state",
                    ))
                }
            };
            let record = HostInteractionNotificationRecord {
                notification_id,
                checkpoint_key,
                record_id,
                payload,
                payload_digest,
                outbox_state,
                claim_token,
                lease_expires_at_ms: lease_expires_at_ms.map(to_u64).transpose()?,
                attempt: to_u64(attempt)?,
                delivered_at_ms: delivered_at_ms.map(to_u64).transpose()?,
                aborted_at_ms: aborted_at_ms.map(to_u64).transpose()?,
                abort_reason,
                last_error,
            };
            record.validate()?;
            Ok(record)
        },
    )
    .transpose()
}

fn sqlite_get_host_interaction_notification(
    store: &SqliteCheckpointStore,
    notification_id: &str,
) -> CheckpointResult<Option<HostInteractionNotificationRecord>> {
    let mut connection = store.lock()?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Deferred)
        .map_err(sqlite_error)?;
    let result = load_notification(&transaction, notification_id)?;
    transaction.commit().map_err(sqlite_error)?;
    Ok(result)
}

fn update_notification(
    transaction: &Transaction<'_>,
    notification: &HostInteractionNotificationRecord,
) -> CheckpointResult<()> {
    notification.validate()?;
    transaction
        .execute(
            "UPDATE host_interaction_notification_outbox SET outbox_state = ?1, claim_token = ?2, lease_expires_at_ms = ?3, attempt = ?4, delivered_at_ms = ?5, aborted_at_ms = ?6, abort_reason = ?7, last_error = ?8 WHERE notification_id = ?9 AND checkpoint_key = ?10 AND payload_digest = ?11",
            params![
                notification.outbox_state.as_str(),
                notification.claim_token,
                notification
                    .lease_expires_at_ms
                    .map(|value| to_i64(value, "lease_expires_at_ms"))
                    .transpose()?,
                to_i64(notification.attempt, "attempt")?,
                notification
                    .delivered_at_ms
                    .map(|value| to_i64(value, "delivered_at_ms"))
                    .transpose()?,
                notification
                    .aborted_at_ms
                    .map(|value| to_i64(value, "aborted_at_ms"))
                    .transpose()?,
                notification.abort_reason,
                notification.last_error,
                notification.notification_id,
                notification.checkpoint_key,
                notification.payload_digest,
            ],
        )
        .map_err(sqlite_error)?;
    Ok(())
}

fn sqlite_reap_host_interaction_record(
    store: &SqliteCheckpointStore,
    record_id: &str,
    checkpoint_key: &str,
    now_ms: u64,
) -> CheckpointResult<bool> {
    let mut connection = store.lock()?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    let mut statement = transaction
        .prepare(
            "SELECT interaction_id FROM host_interaction_records WHERE record_id = ?1 AND checkpoint_key = ?2",
        )
        .map_err(sqlite_error)?;
    let interaction_id = statement
        .query_row(params![record_id, checkpoint_key], |row| row.get::<_, String>(0))
        .optional()
        .map_err(sqlite_error)?;
    drop(statement);
    let Some(interaction_id) = interaction_id else {
        transaction.commit().map_err(sqlite_error)?;
        return Ok(false);
    };
    let Some(mut record) =
        load_host_record_by_interaction(&transaction, checkpoint_key, &interaction_id)?
    else {
        transaction.commit().map_err(sqlite_error)?;
        return Ok(false);
    };
    let Some(checkpoint) = load_row_transaction(&transaction, checkpoint_key)? else {
        transaction.commit().map_err(sqlite_error)?;
        return Ok(false);
    };
    if record.state != "resolved_claimed"
        || record.claim_token.is_none()
        || record
            .lease_expires_at_ms
            .is_none_or(|lease| lease > now_ms)
        || checkpoint.status != crate::checkpoint::CheckpointStatus::Running
        || checkpoint.claim_token.as_deref() != record.claim_token.as_deref()
        || checkpoint.claim_token.is_none()
        || checkpoint
            .lease_expires_at_ms
            .is_none_or(|lease| lease > now_ms)
    {
        transaction.commit().map_err(sqlite_error)?;
        return Ok(false);
    }
    let claim_token = record.claim_token.clone().expect("checked above");
    let checkpoint_cas = transaction
        .execute(
            "UPDATE checkpoints SET lease_expires_at_ms = lease_expires_at_ms WHERE checkpoint_key = ?1 AND claim_token = ?2 AND lease_expires_at_ms <= ?3",
            params![checkpoint_key, claim_token, to_i64(now_ms, "now_ms")?],
        )
        .map_err(sqlite_error)?;
    if checkpoint_cas != 1 {
        transaction.commit().map_err(sqlite_error)?;
        return Ok(false);
    }
    record.state = "resolved_pending".to_string();
    record.claim_token = None;
    record.lease_expires_at_ms = None;
    record.last_error = Some("host_interaction_response_claim_expired".to_string());
    record.validate()?;
    let changed = transaction
        .execute(
            "UPDATE host_interaction_records SET state = ?1, claim_token = NULL, lease_expires_at_ms = NULL, last_error = ?2 WHERE record_id = ?3 AND checkpoint_key = ?4 AND state = 'resolved_claimed' AND claim_token = ?5",
            params![record.state, record.last_error, record_id, checkpoint_key, claim_token],
        )
        .map_err(sqlite_error)?;
    if changed != 1 {
        transaction.commit().map_err(sqlite_error)?;
        return Ok(false);
    }
    transaction.commit().map_err(sqlite_error)?;
    Ok(true)
}

fn sqlite_claim_host_interaction_notification(
    store: &SqliteCheckpointStore,
    notification_id: &str,
    payload_digest: &str,
    claim_token: &str,
    lease_expires_at_ms: u64,
    now_ms: u64,
) -> CheckpointResult<Option<HostInteractionNotificationRecord>> {
    if claim_token.trim().is_empty() || lease_expires_at_ms <= now_ms {
        return Err(CheckpointError::new(
            "notification_conflict",
            "notification claim token must be non-empty and lease must be in the future",
        ));
    }
    let mut connection = store.lock()?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    let Some(mut notification) = load_notification(&transaction, notification_id)? else {
        transaction.commit().map_err(sqlite_error)?;
        return Ok(None);
    };
    if notification.payload_digest != payload_digest {
        return Err(CheckpointError::new(
            "notification_conflict",
            "notification payload digest does not match",
        ));
    }
    let claimable = notification.outbox_state == NotificationOutboxState::Pending
        || (notification.outbox_state == NotificationOutboxState::Claimed
            && notification
                .lease_expires_at_ms
                .is_some_and(|lease| lease <= now_ms));
    if !claimable {
        transaction.commit().map_err(sqlite_error)?;
        return Ok(None);
    }
    notification.outbox_state = NotificationOutboxState::Claimed;
    notification.claim_token = Some(claim_token.to_string());
    notification.lease_expires_at_ms = Some(lease_expires_at_ms);
    notification.attempt = notification.attempt.saturating_add(1);
    notification.delivered_at_ms = None;
    notification.aborted_at_ms = None;
    notification.abort_reason = None;
    notification.last_error = None;
    update_notification(&transaction, &notification)?;
    transaction.commit().map_err(sqlite_error)?;
    Ok(Some(notification))
}

#[allow(clippy::too_many_arguments)]
fn sqlite_complete_host_interaction_notification(
    store: &SqliteCheckpointStore,
    notification_id: &str,
    payload_digest: &str,
    claim_token: &str,
    attempt: u64,
    outcome: &str,
    now_ms: u64,
    error: Option<&str>,
) -> CheckpointResult<Option<HostInteractionNotificationRecord>> {
    let mut connection = store.lock()?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    let Some(mut notification) = load_notification(&transaction, notification_id)? else {
        transaction.commit().map_err(sqlite_error)?;
        return Ok(None);
    };
    if notification.payload_digest != payload_digest
        || notification.outbox_state != NotificationOutboxState::Claimed
        || notification.claim_token.as_deref() != Some(claim_token)
        || notification.attempt != attempt
    {
        transaction.commit().map_err(sqlite_error)?;
        return Ok(None);
    }
    match outcome {
        "delivered" => {
            notification.outbox_state = NotificationOutboxState::Delivered;
            notification.delivered_at_ms = Some(now_ms);
            notification.last_error = None;
        }
        "ambiguous" => {
            notification.outbox_state = NotificationOutboxState::Ambiguous;
            notification.last_error = error.map(str::to_string);
        }
        _ => {
            return Err(CheckpointError::new(
                "notification_conflict",
                "notification completion outcome is invalid",
            ))
        }
    }
    notification.claim_token = None;
    notification.lease_expires_at_ms = None;
    update_notification(&transaction, &notification)?;
    transaction.commit().map_err(sqlite_error)?;
    Ok(Some(notification))
}

fn sqlite_reconcile_host_interaction_notification(
    store: &SqliteCheckpointStore,
    notification_id: &str,
    payload_digest: &str,
    outcome: &str,
    now_ms: u64,
    abort_reason: Option<&str>,
) -> CheckpointResult<Option<HostInteractionNotificationRecord>> {
    if !matches!(outcome, "delivered" | "retry" | "abort")
        || (outcome == "abort"
            && abort_reason.is_none_or(|reason| reason.trim().is_empty()))
    {
        return Err(CheckpointError::new(
            "notification_conflict",
            "notification reconciliation payload is invalid",
        ));
    }
    let mut connection = store.lock()?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    let Some(mut notification) = load_notification(&transaction, notification_id)? else {
        transaction.commit().map_err(sqlite_error)?;
        return Ok(None);
    };
    if notification.payload_digest != payload_digest {
        transaction.commit().map_err(sqlite_error)?;
        return Err(CheckpointError::new(
            "notification_conflict",
            "notification payload digest does not match",
        ));
    }
    let target = match outcome {
        "delivered" => NotificationOutboxState::Delivered,
        "retry" => NotificationOutboxState::Pending,
        "abort" => NotificationOutboxState::Aborted,
        _ => {
            transaction.commit().map_err(sqlite_error)?;
            return Err(CheckpointError::new(
                "notification_conflict",
                "notification reconciliation outcome is invalid",
            ));
        }
    };
    if notification.outbox_state == target {
        transaction.commit().map_err(sqlite_error)?;
        return Ok(Some(notification));
    }
    if notification.outbox_state != NotificationOutboxState::Ambiguous {
        transaction.commit().map_err(sqlite_error)?;
        return Err(CheckpointError::new(
            "notification_stale",
            "notification is not ambiguous",
        ));
    }
    match target {
        NotificationOutboxState::Delivered => {
            notification.outbox_state = NotificationOutboxState::Delivered;
            notification.delivered_at_ms = Some(now_ms);
            notification.last_error = None;
        }
        NotificationOutboxState::Pending => {
            notification.outbox_state = NotificationOutboxState::Pending;
            notification.last_error = None;
        }
        NotificationOutboxState::Aborted => {
            let reason = abort_reason
                .filter(|reason| !reason.trim().is_empty())
                .ok_or_else(|| {
                    CheckpointError::new(
                        "notification_conflict",
                        "abort notification requires a reason",
                    )
                })?;
            notification.outbox_state = NotificationOutboxState::Aborted;
            notification.aborted_at_ms = Some(now_ms);
            notification.abort_reason = Some(reason.to_string());
            notification.last_error = None;
        }
        NotificationOutboxState::Claimed | NotificationOutboxState::Ambiguous => unreachable!(),
    }
    notification.claim_token = None;
    notification.lease_expires_at_ms = None;
    update_notification(&transaction, &notification)?;
    transaction.commit().map_err(sqlite_error)?;
    Ok(Some(notification))
}


include!("sqlite_interaction_wake.rs");
