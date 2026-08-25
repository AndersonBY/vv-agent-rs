fn redis_wake_outbox_matches_receipt(
    wake: &RedisControllerWakeOutbox,
    receipt: &ControllerCommandReceipt,
) -> CheckpointResult<()> {
    if wake.command_id != receipt.command_id
        || wake.command_digest != receipt.command_digest
        || wake.outbox_action != receipt.outbox_action
        || wake.outbox_destination != receipt.outbox_destination
        || wake.outbox_state != receipt.outbox_state
        || wake.attempt != receipt.outbox_attempt
    {
        return Err(CheckpointError::new(
            "controller_command_conflict",
            "controller receipt and wake outbox conflict",
        ));
    }
    Ok(())
}

fn redis_controller_wake(
    command: &ControllerCommand,
    receipt: &ControllerCommandReceipt,
    checkpoint: &Checkpoint,
) -> CheckpointResult<crate::checkpoint::ControllerCommandWake> {
    let logical_cycle = match &command.command {
        crate::checkpoint::ControllerCommandVariant::HostInteractionResponse {
            logical_cycle, ..
        } => *logical_cycle,
        _ => checkpoint.cycle_index.checked_add(1).ok_or_else(|| {
            CheckpointError::new(
                "controller_command_invalid_state",
                "controller wake logical cycle overflow",
            )
        })?,
    };
    if receipt.outbox_action == "none" {
        Ok(crate::checkpoint::ControllerCommandWake::none())
    } else {
        Ok(crate::checkpoint::ControllerCommandWake::recovery(logical_cycle))
    }
}

fn redis_claim_controller_command_wake(
    store: &RedisCheckpointStore,
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
    crate::checkpoint::validate_sha256(command_digest, "command_digest")?;
    let receipt_key = RedisCheckpointStore::controller_command_key(command_id);
    let command_key = RedisCheckpointStore::controller_command_payload_key(command_id);
    let outbox_key = RedisCheckpointStore::controller_command_outbox_key(command_id);
    store.controller_transaction(&[receipt_key.as_str(), command_key.as_str(), outbox_key.as_str()], |connection, pipeline| {
        let Some(raw) = connection
            .get::<_, Option<String>>(&receipt_key)
            .map_err(redis_error)?
        else {
            return Ok(Some(None));
        };
        let receipt = redis_decode_controller_receipt(&raw)?;
        let _command = redis_load_controller_command(connection, command_id, command_digest)?;
        if receipt.command_id != command_id || receipt.command_digest != command_digest {
            return Err(CheckpointError::new(
                "controller_command_conflict",
                "controller wake identity or digest does not match receipt",
            ));
        }
        let raw_outbox = connection
            .get::<_, Option<String>>(&outbox_key)
            .map_err(redis_error)?;
        let raw_outbox = raw_outbox.ok_or_else(|| {
            CheckpointError::new(
                "controller_command_conflict",
                "controller wake outbox is missing",
            )
        })?;
        let wake = RedisControllerWakeOutbox::from_value(&serde_json::from_str(&raw_outbox)?)?;
        redis_wake_outbox_matches_receipt(&wake, &receipt)?;
        if wake.outbox_action == "none" || wake.outbox_state == "delivered" {
            return Ok(Some(Some(receipt)));
        }
        if wake.outbox_state == "ambiguous" {
            return Err(CheckpointError::new(
                "controller_command_stale",
                "controller wake requires reconciliation",
            ));
        }
        if wake.outbox_state == "claimed" {
            if wake.claim_token.as_deref() == Some(claim_token) {
                return Ok(Some(Some(receipt)));
            }
            if wake.lease_expires_at_ms.is_some_and(|expiry| expiry > now_ms) {
                return Err(CheckpointError::new(
                    "controller_command_stale",
                    "controller wake is claimed by another owner",
                ));
            }
        }
        if wake.outbox_state != "pending" && wake.outbox_state != "claimed" {
            return Ok(Some(None));
        }
        let mut updated = receipt;
        updated.outbox_state = "claimed".to_string();
        updated.outbox_attempt = updated.outbox_attempt.saturating_add(1);
        updated.validate()?;
        let mut updated_wake = RedisControllerWakeOutbox::from_receipt_unchecked(&updated)?;
        updated_wake.claim_token = Some(claim_token.to_string());
        updated_wake.lease_expires_at_ms = Some(lease_expires_at_ms);
        updated_wake.validate()?;
        pipeline
            .set(
                &receipt_key,
                redis_encode_controller_receipt(&updated)?,
            )
            .ignore();
        pipeline
            .set(
                &outbox_key,
                serde_json::to_string(&updated_wake.to_value())?,
            )
            .ignore();
        Ok(Some(Some(updated)))
    })
}

#[allow(clippy::too_many_arguments)]
fn redis_complete_controller_command_wake(
    store: &RedisCheckpointStore,
    command_id: &str,
    command_digest: &str,
    claim_token: &str,
    attempt: u64,
    outcome: &str,
    now_ms: u64,
    error: Option<&str>,
) -> CheckpointResult<Option<ControllerCommandReceipt>> {
    if claim_token.trim().is_empty() || attempt == 0 {
        return Err(CheckpointError::new(
            "controller_command_outbox_invalid",
            "wake completion claim token and attempt are invalid",
        ));
    }
    crate::checkpoint::validate_sha256(command_digest, "command_digest")?;
    if !matches!(outcome, "delivered" | "ambiguous") {
        return Err(CheckpointError::new(
            "controller_command_outbox_invalid",
            "wake completion outcome is invalid",
        ));
    }
    let receipt_key = RedisCheckpointStore::controller_command_key(command_id);
    let command_key = RedisCheckpointStore::controller_command_payload_key(command_id);
    let outbox_key = RedisCheckpointStore::controller_command_outbox_key(command_id);
    store.controller_transaction(&[receipt_key.as_str(), command_key.as_str(), outbox_key.as_str()], |connection, pipeline| {
        let Some(raw) = connection
            .get::<_, Option<String>>(&receipt_key)
            .map_err(redis_error)?
        else {
            return Ok(Some(None));
        };
        let receipt = redis_decode_controller_receipt(&raw)?;
        let _command = redis_load_controller_command(connection, command_id, command_digest)?;
        if receipt.command_id != command_id || receipt.command_digest != command_digest {
            return Err(CheckpointError::new(
                "controller_command_conflict",
                "controller wake digest does not match receipt",
            ));
        }
        let raw_outbox = connection
            .get::<_, Option<String>>(&outbox_key)
            .map_err(redis_error)?
            .ok_or_else(|| {
                CheckpointError::new(
                    "controller_command_conflict",
                    "controller wake outbox is missing",
                )
            })?;
        let wake = RedisControllerWakeOutbox::from_value(&serde_json::from_str(&raw_outbox)?)?;
        redis_wake_outbox_matches_receipt(&wake, &receipt)?;
        if wake.outbox_state == "delivered" || wake.outbox_state == "ambiguous" {
            if wake.outbox_state == outcome {
                return Ok(Some(Some(receipt)));
            }
            return Err(CheckpointError::new(
                "controller_command_stale",
                "controller wake has already completed",
            ));
        }
        if receipt.outbox_state != "claimed"
            || wake.attempt != attempt
            || wake.claim_token.as_deref() != Some(claim_token)
        {
            return Err(CheckpointError::new(
                "controller_command_stale",
                "controller wake owner or attempt is stale",
            ));
        }
        let mut updated = receipt;
        updated.outbox_state = outcome.to_string();
        updated.validate()?;
        let mut updated_wake = RedisControllerWakeOutbox::from_receipt_unchecked(&updated)?;
        updated_wake.delivered_at_ms = (outcome == "delivered").then_some(now_ms);
        updated_wake.last_error = error.map(crate::checkpoint::sanitize_host_text);
        updated_wake.validate()?;
        pipeline
            .set(
                &receipt_key,
                redis_encode_controller_receipt(&updated)?,
            )
            .ignore();
        pipeline
            .set(
                &outbox_key,
                serde_json::to_string(&updated_wake.to_value())?,
            )
            .ignore();
        Ok(Some(Some(updated)))
    })
}

fn redis_reconcile_controller_command_wake(
    store: &RedisCheckpointStore,
    command_id: &str,
    command_digest: &str,
    outcome: &str,
    now_ms: u64,
) -> CheckpointResult<Option<ControllerCommandReceipt>> {
    if !matches!(outcome, "delivered" | "retry") {
        return Err(CheckpointError::new(
            "controller_command_outbox_invalid",
            "wake reconciliation outcome is invalid",
        ));
    }
    crate::checkpoint::validate_sha256(command_digest, "command_digest")?;
    let receipt_key = RedisCheckpointStore::controller_command_key(command_id);
    let command_key = RedisCheckpointStore::controller_command_payload_key(command_id);
    let outbox_key = RedisCheckpointStore::controller_command_outbox_key(command_id);
    store.controller_transaction(&[receipt_key.as_str(), command_key.as_str(), outbox_key.as_str()], |connection, pipeline| {
        let Some(raw) = connection
            .get::<_, Option<String>>(&receipt_key)
            .map_err(redis_error)?
        else {
            return Ok(Some(None));
        };
        let receipt = redis_decode_controller_receipt(&raw)?;
        let _command = redis_load_controller_command(connection, command_id, command_digest)?;
        if receipt.command_id != command_id || receipt.command_digest != command_digest {
            return Err(CheckpointError::new(
                "controller_command_conflict",
                "controller wake digest does not match receipt",
            ));
        }
        let raw_outbox = connection
            .get::<_, Option<String>>(&outbox_key)
            .map_err(redis_error)?
            .ok_or_else(|| {
                CheckpointError::new(
                    "controller_command_conflict",
                    "controller wake outbox is missing",
                )
            })?;
        let wake = RedisControllerWakeOutbox::from_value(&serde_json::from_str(&raw_outbox)?)?;
        redis_wake_outbox_matches_receipt(&wake, &receipt)?;
        let target = if outcome == "delivered" {
            "delivered"
        } else {
            "pending"
        };
        if wake.outbox_state == target {
            return Ok(Some(Some(receipt)));
        }
        if wake.outbox_state != "ambiguous" {
            return Err(CheckpointError::new(
                "controller_command_stale",
                "controller wake is not ambiguous",
            ));
        }
        let mut updated = receipt;
        updated.outbox_state = target.to_string();
        updated.validate()?;
        let mut updated_wake = RedisControllerWakeOutbox::from_receipt_unchecked(&updated)?;
        updated_wake.delivered_at_ms = (target == "delivered").then_some(now_ms);
        updated_wake.last_error = None;
        updated_wake.validate()?;
        pipeline
            .set(
                &receipt_key,
                redis_encode_controller_receipt(&updated)?,
            )
            .ignore();
        pipeline
            .set(
                &outbox_key,
                serde_json::to_string(&updated_wake.to_value())?,
            )
            .ignore();
        Ok(Some(Some(updated)))
    })
}

fn redis_reap_controller_command_wake(
    store: &RedisCheckpointStore,
    command_id: &str,
    command_digest: &str,
    now_ms: u64,
) -> CheckpointResult<bool> {
    crate::checkpoint::validate_sha256(command_digest, "command_digest")?;
    let receipt_key = RedisCheckpointStore::controller_command_key(command_id);
    let command_key = RedisCheckpointStore::controller_command_payload_key(command_id);
    let outbox_key = RedisCheckpointStore::controller_command_outbox_key(command_id);
    store.controller_transaction(&[receipt_key.as_str(), command_key.as_str(), outbox_key.as_str()], |connection, pipeline| {
        let Some(raw) = connection
            .get::<_, Option<String>>(&receipt_key)
            .map_err(redis_error)?
        else {
            return Ok(Some(false));
        };
        let receipt = redis_decode_controller_receipt(&raw)?;
        let _command = redis_load_controller_command(connection, command_id, command_digest)?;
        if receipt.command_id != command_id || receipt.command_digest != command_digest {
            return Err(CheckpointError::new(
                "controller_command_conflict",
                "controller wake digest does not match receipt",
            ));
        }
        let raw_outbox = connection
            .get::<_, Option<String>>(&outbox_key)
            .map_err(redis_error)?
            .ok_or_else(|| {
                CheckpointError::new(
                    "controller_command_conflict",
                    "controller wake outbox is missing",
                )
            })?;
        let wake = RedisControllerWakeOutbox::from_value(&serde_json::from_str(&raw_outbox)?)?;
        redis_wake_outbox_matches_receipt(&wake, &receipt)?;
        if wake.outbox_state != "claimed"
            || wake.lease_expires_at_ms.is_none_or(|expiry| expiry > now_ms)
        {
            return Ok(Some(false));
        }
        let mut updated = receipt;
        updated.outbox_state = "pending".to_string();
        updated.validate()?;
        let mut updated_wake = RedisControllerWakeOutbox::from_receipt_unchecked(&updated)?;
        updated_wake.last_error = Some("controller_wake_claim_expired".to_string());
        updated_wake.validate()?;
        pipeline
            .set(
                &receipt_key,
                redis_encode_controller_receipt(&updated)?,
            )
            .ignore();
        pipeline
            .set(
                &outbox_key,
                serde_json::to_string(&updated_wake.to_value())?,
            )
            .ignore();
        Ok(Some(true))
    })
}
