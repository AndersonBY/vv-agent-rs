fn redis_claim_and_consume_host_interaction_response(
    store: &RedisCheckpointStore,
    envelope: HostInteractionRecoveryEnvelope,
) -> CheckpointResult<HostInteractionRecoveryResult> {
    envelope.validate()?;
    let data_key = RedisCheckpointStore::data_key(&envelope.checkpoint_key);
    let lease_key = RedisCheckpointStore::lease_key(&envelope.checkpoint_key);
    let record_key = RedisCheckpointStore::host_interaction_key(
        &envelope.checkpoint_key,
        &envelope.interaction_id,
    );
    let receipt_set_key =
        RedisCheckpointStore::controller_receipts_checkpoint_set_key(&envelope.checkpoint_key);
    let watch_keys = [
        data_key.as_str(),
        lease_key.as_str(),
        record_key.as_str(),
        receipt_set_key.as_str(),
    ];
    store.controller_transaction(&watch_keys, |connection, pipeline| {
        let raw = connection
            .get::<_, Option<String>>(&data_key)
            .map_err(redis_error)?
            .ok_or_else(|| {
                CheckpointError::new(
                    "host_interaction_recovery_stale",
                    "checkpoint does not exist",
                )
            })?;
        let current = decode_storage(
            &raw,
            connection
                .get::<_, Option<u64>>(&lease_key)
                .map_err(redis_error)?,
        )?;
        let raw_record = connection
            .get::<_, Option<String>>(&record_key)
            .map_err(redis_error)?
            .ok_or_else(|| {
                CheckpointError::new(
                    "host_interaction_recovery_stale",
                    "host interaction record does not exist",
                )
            })?;
        let record = redis_decode_host_record(&raw_record)?;
        if record.record_id != envelope.record_id {
            return Err(CheckpointError::new(
                "host_interaction_recovery_stale",
                "record identity does not match envelope",
            ));
        }
        if record.state == "consumed" {
            if !redis_recovery_identity_matches(&current, &record, &envelope) {
                return Err(CheckpointError::new(
                    "host_interaction_recovery_stale",
                    "recovery envelope does not match consumed record",
                ));
            }
            return Ok(Some(redis_recovery_result(
                "replayed",
                &current,
                &record,
                if current.claim_token.is_some() {
                    "retained"
                } else {
                    "released"
                },
            )?));
        }
        if !redis_recovery_identity_matches(&current, &record, &envelope)
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
        let claimed_cycle = current.cycle_index.checked_add(1).ok_or_else(|| {
            CheckpointError::new("checkpoint_cycle_invalid", "cycle index overflow")
        })?;
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
        updated.lease_expires_at_ms = Some(redis_recovery_lease_deadline());
        updated.revision = envelope.expected_revision.checked_add(1).ok_or_else(|| {
            CheckpointError::new("checkpoint_revision_overflow", "revision overflow")
        })?;
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
        updated
            .event_outbox
            .push(crate::runtime::state::EventOutboxEntry::pending(
                event.event_id.as_str(),
                serde_json::to_value(&event)?,
            )?);
        updated.validate()?;
        let mut consumed = record;
        consumed.state = "consumed".to_string();
        consumed.consumed_revision = Some(updated.revision);
        consumed.claim_token = None;
        consumed.lease_expires_at_ms = None;
        consumed.validate()?;
        pipeline
            .set(
                &data_key,
                checkpoint_to_json(&updated, MAX_EXTENSION_STATE_BYTES)?,
            )
            .ignore();
        if let Some(lease) = updated.lease_expires_at_ms {
            pipeline.set(&lease_key, lease).ignore();
        } else {
            pipeline.del(&lease_key).ignore();
        }
        pipeline
            .set(&record_key, serde_json::to_string(&consumed.to_value())?)
            .ignore();
        Ok(Some(redis_recovery_result(
            "applied", &updated, &consumed, "retained",
        )?))
    })
}

fn redis_recovery_result(
    kind: &str,
    checkpoint: &Checkpoint,
    record: &HostInteractionRecord,
    claim_state: &str,
) -> CheckpointResult<HostInteractionRecoveryResult> {
    let result = HostInteractionRecoveryResult {
        schema_version: crate::checkpoint::HOST_INTERACTION_RECOVERY_RESULT_SCHEMA.to_string(),
        kind: kind.to_string(),
        record_id: record.record_id.clone(),
        checkpoint_revision: Some(checkpoint.revision),
        consumed_revision: record.consumed_revision,
        claim_mode: "recovery".to_string(),
        resume_attempt: Some(checkpoint.resume_attempt),
        injection_count: 1,
        checkpoint_execution_claim_state: claim_state.to_string(),
        error: None,
    };
    result.validate()?;
    Ok(result)
}

fn redis_recovery_identity_matches(
    checkpoint: &Checkpoint,
    record: &HostInteractionRecord,
    envelope: &HostInteractionRecoveryEnvelope,
) -> bool {
    checkpoint.checkpoint_key == envelope.checkpoint_key
        && checkpoint.root_run_id == envelope.run_id
        && checkpoint.trace_id == envelope.trace_id
        && record.checkpoint_key == envelope.checkpoint_key
        && record.record_id == envelope.record_id
        && record.logical_cycle == envelope.logical_cycle
        && record.interaction_id == envelope.interaction_id
        && record.request.operation_id == envelope.operation_id
        && record.request.tool_call_id == envelope.tool_call_id
        && record.request_digest == envelope.request_digest
        && record.command_id.as_deref() == Some(envelope.command_id.as_str())
}

fn redis_recovery_lease_deadline() -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    now.saturating_add(5 * 60 * 1_000)
}

fn redis_host_interaction_outcome(
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

fn redis_decode_host_record(raw: &str) -> CheckpointResult<HostInteractionRecord> {
    HostInteractionRecord::from_value(&serde_json::from_str(raw)?)
}

fn redis_encode_notification(
    notification: &HostInteractionNotificationRecord,
) -> CheckpointResult<String> {
    Ok(serde_json::to_string(&serde_json::json!({
        "notification_id": notification.notification_id,
        "checkpoint_key": notification.checkpoint_key,
        "record_id": notification.record_id,
        "payload": notification.payload.to_value(),
        "payload_digest": notification.payload_digest,
        "outbox_state": notification.outbox_state.as_str(),
        "claim_token": notification.claim_token,
        "lease_expires_at_ms": notification.lease_expires_at_ms,
        "attempt": notification.attempt,
        "delivered_at_ms": notification.delivered_at_ms,
        "aborted_at_ms": notification.aborted_at_ms,
        "abort_reason": notification.abort_reason,
        "last_error": notification.last_error,
    }))?)
}

fn redis_decode_notification(raw: &str) -> CheckpointResult<HostInteractionNotificationRecord> {
    let value: Value = serde_json::from_str(raw).map_err(|error| {
        CheckpointError::new("notification_conflict", error.to_string())
    })?;
    let object = value.as_object().ok_or_else(|| {
        CheckpointError::new(
            "notification_conflict",
            "notification record must be an object",
        )
    })?;
    const FIELDS: &[&str] = &[
        "notification_id",
        "checkpoint_key",
        "record_id",
        "payload",
        "payload_digest",
        "outbox_state",
        "claim_token",
        "lease_expires_at_ms",
        "attempt",
        "delivered_at_ms",
        "aborted_at_ms",
        "abort_reason",
        "last_error",
    ];
    if object.len() != FIELDS.len() || object.keys().any(|key| !FIELDS.contains(&key.as_str())) {
        return Err(CheckpointError::new(
            "notification_conflict",
            "notification record has unknown or missing fields",
        ));
    }
    let required_string = |name: &str| -> CheckpointResult<String> {
        object
            .get(name)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
            .ok_or_else(|| {
                CheckpointError::new(
                    "notification_conflict",
                    format!("notification field {name} must be a non-empty string"),
                )
            })
    };
    let nullable_string = |name: &str| -> CheckpointResult<Option<String>> {
        let value = object.get(name).ok_or_else(|| {
            CheckpointError::new(
                "notification_conflict",
                format!("notification field {name} is missing"),
            )
        })?;
        if value.is_null() {
            Ok(None)
        } else {
            value
                .as_str()
                .filter(|value| !value.is_empty())
                .map(|value| Some(value.to_string()))
                .ok_or_else(|| {
                    CheckpointError::new(
                        "notification_conflict",
                        format!("notification field {name} must be string or null"),
                    )
                })
        }
    };
    let nullable_u64 = |name: &str| -> CheckpointResult<Option<u64>> {
        let value = object.get(name).ok_or_else(|| {
            CheckpointError::new(
                "notification_conflict",
                format!("notification field {name} is missing"),
            )
        })?;
        if value.is_null() {
            Ok(None)
        } else {
            value.as_u64().map(Some).ok_or_else(|| {
                CheckpointError::new(
                    "notification_conflict",
                    format!("notification field {name} must be an unsigned integer or null"),
                )
            })
        }
    };
    let attempt = object
        .get("attempt")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            CheckpointError::new(
                "notification_conflict",
                "notification attempt must be an unsigned integer",
            )
        })?;
    let state = match object.get("outbox_state").and_then(Value::as_str) {
        Some("pending") => NotificationOutboxState::Pending,
        Some("claimed") => NotificationOutboxState::Claimed,
        Some("delivered") => NotificationOutboxState::Delivered,
        Some("ambiguous") => NotificationOutboxState::Ambiguous,
        Some("aborted") => NotificationOutboxState::Aborted,
        _ => {
            return Err(CheckpointError::new(
                "notification_conflict",
                "notification outbox state is invalid",
            ))
        }
    };
    let record = HostInteractionNotificationRecord {
        notification_id: required_string("notification_id")?,
        checkpoint_key: required_string("checkpoint_key")?,
        record_id: required_string("record_id")?,
        payload: HostInteractionNotificationPayload::from_value(
            object.get("payload").ok_or_else(|| {
                CheckpointError::new("notification_conflict", "notification payload is missing")
            })?,
        )?,
        payload_digest: required_string("payload_digest")?,
        outbox_state: state,
        claim_token: nullable_string("claim_token")?,
        lease_expires_at_ms: nullable_u64("lease_expires_at_ms")?,
        attempt,
        delivered_at_ms: nullable_u64("delivered_at_ms")?,
        aborted_at_ms: nullable_u64("aborted_at_ms")?,
        abort_reason: nullable_string("abort_reason")?,
        last_error: nullable_string("last_error")?,
    };
    record.validate()?;
    Ok(record)
}

fn redis_get_host_interaction_notification(
    store: &RedisCheckpointStore,
    notification_id: &str,
) -> CheckpointResult<Option<HostInteractionNotificationRecord>> {
    let key = RedisCheckpointStore::host_interaction_notification_key(notification_id);
    let mut connection = store.lock()?;
    let raw = connection
        .get::<_, Option<String>>(&key)
        .map_err(redis_error)?;
    raw.as_deref().map(redis_decode_notification).transpose()
}

fn redis_sanitize_public_prompt(prompt: &str) -> String {
    crate::checkpoint::sanitize_public_text(prompt)
}

fn redis_claim_host_interaction_notification(
    store: &RedisCheckpointStore,
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
    let key = RedisCheckpointStore::host_interaction_notification_key(notification_id);
    store.controller_transaction(&[key.as_str()], |connection, pipeline| {
        let Some(raw) = connection
            .get::<_, Option<String>>(&key)
            .map_err(redis_error)?
        else {
            return Ok(Some(None));
        };
        let mut notification = redis_decode_notification(&raw)?;
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
            return Ok(Some(None));
        }
        notification.outbox_state = NotificationOutboxState::Claimed;
        notification.claim_token = Some(claim_token.to_string());
        notification.lease_expires_at_ms = Some(lease_expires_at_ms);
        notification.attempt = notification.attempt.saturating_add(1);
        notification.delivered_at_ms = None;
        notification.aborted_at_ms = None;
        notification.abort_reason = None;
        notification.last_error = None;
        pipeline
            .set(&key, redis_encode_notification(&notification)?)
            .ignore();
        Ok(Some(Some(notification)))
    })
}

#[allow(clippy::too_many_arguments)]
fn redis_complete_host_interaction_notification(
    store: &RedisCheckpointStore,
    notification_id: &str,
    payload_digest: &str,
    claim_token: &str,
    attempt: u64,
    outcome: &str,
    now_ms: u64,
    error: Option<&str>,
) -> CheckpointResult<Option<HostInteractionNotificationRecord>> {
    let key = RedisCheckpointStore::host_interaction_notification_key(notification_id);
    store.controller_transaction(&[key.as_str()], |connection, pipeline| {
        let Some(raw) = connection
            .get::<_, Option<String>>(&key)
            .map_err(redis_error)?
        else {
            return Ok(Some(None));
        };
        let mut notification = redis_decode_notification(&raw)?;
        if notification.payload_digest != payload_digest
            || notification.outbox_state != NotificationOutboxState::Claimed
            || notification.claim_token.as_deref() != Some(claim_token)
            || notification.attempt != attempt
        {
            return Ok(Some(None));
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
        pipeline
            .set(&key, redis_encode_notification(&notification)?)
            .ignore();
        Ok(Some(Some(notification)))
    })
}

fn redis_reconcile_host_interaction_notification(
    store: &RedisCheckpointStore,
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
    let key = RedisCheckpointStore::host_interaction_notification_key(notification_id);
    store.controller_transaction(&[key.as_str()], |connection, pipeline| {
        let Some(raw) = connection
            .get::<_, Option<String>>(&key)
            .map_err(redis_error)?
        else {
            return Ok(Some(None));
        };
        let mut notification = redis_decode_notification(&raw)?;
        if notification.payload_digest != payload_digest {
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
                return Err(CheckpointError::new(
                    "notification_conflict",
                    "notification reconciliation outcome is invalid",
                ))
            }
        };
        if notification.outbox_state == target {
            return Ok(Some(Some(notification)));
        }
        if notification.outbox_state != NotificationOutboxState::Ambiguous {
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
            NotificationOutboxState::Claimed | NotificationOutboxState::Ambiguous => {
                unreachable!()
            }
        }
        notification.claim_token = None;
        notification.lease_expires_at_ms = None;
        pipeline
            .set(&key, redis_encode_notification(&notification)?)
            .ignore();
        Ok(Some(Some(notification)))
    })
}

fn redis_reap_host_interaction_record(
    store: &RedisCheckpointStore,
    record_id: &str,
    checkpoint_key: &str,
    now_ms: u64,
) -> CheckpointResult<bool> {
    // The canonical key is derived from interaction_id, not record_id.  Scan
    // only the checkpoint's durable set to resolve that identity before the
    // CAS; never scan unrelated checkpoints.
    let mut connection = store.lock()?;
    let keys = connection
        .smembers::<_, Vec<String>>(
            &RedisCheckpointStore::host_interactions_checkpoint_set_key(checkpoint_key),
        )
        .map_err(redis_error)?;
    let candidates = keys
        .into_iter()
        .filter(|key| key.starts_with(HOST_INTERACTION_PREFIX))
        .collect::<Vec<_>>();
    drop(connection);
    for record_key in candidates {
        let data_key = RedisCheckpointStore::data_key(checkpoint_key);
        let lease_key = RedisCheckpointStore::lease_key(checkpoint_key);
        let watch_keys = [record_key.as_str(), data_key.as_str(), lease_key.as_str()];
        let reaped = store.controller_transaction(&watch_keys, |connection, pipeline| {
            let Some(raw) = connection
                .get::<_, Option<String>>(&record_key)
                .map_err(redis_error)?
            else {
                return Ok(Some(false));
            };
            let mut record = redis_decode_host_record(&raw)?;
            let Some(checkpoint_raw) = connection
                .get::<_, Option<String>>(&data_key)
                .map_err(redis_error)?
            else {
                return Ok(Some(false));
            };
            let checkpoint = decode_storage(
                &checkpoint_raw,
                connection
                    .get::<_, Option<u64>>(&lease_key)
                    .map_err(redis_error)?,
            )?;
            if record.record_id != record_id
                || record.checkpoint_key != checkpoint_key
                || record.state != "resolved_claimed"
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
                return Ok(Some(false));
            }
            record.state = "resolved_pending".to_string();
            record.claim_token = None;
            record.lease_expires_at_ms = None;
            record.last_error = Some("host_interaction_response_claim_expired".to_string());
            record.validate()?;
            pipeline
                .set(&record_key, serde_json::to_string(&record.to_value())?)
                .ignore();
            Ok(Some(true))
        })?;
        if reaped {
            return Ok(true);
        }
    }
    Ok(false)
}
