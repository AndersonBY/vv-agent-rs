fn redis_produce_host_interaction(
    store: &RedisCheckpointStore,
    request: HostInteractionRequest,
    context: &crate::checkpoint::HostInteractionAdmissionContext,
) -> CheckpointResult<HostInteractionOutcome> {
    request.validate()?;
    context.validate()?;
    let context_is_live = context.validate_live_lease().is_ok();
    let checkpoint_key = context.checkpoint_key.clone();
    let data_key = RedisCheckpointStore::data_key(&checkpoint_key);
    let lease_key = RedisCheckpointStore::lease_key(&checkpoint_key);
    let record_key =
        RedisCheckpointStore::host_interaction_key(&checkpoint_key, &request.interaction_id);
    let record_id = record_id_for(&checkpoint_key, &request);
    let notification_id = notification_id_for(&record_id);
    let notification_key =
        RedisCheckpointStore::host_interaction_notification_key(&notification_id);
    let host_set_key = RedisCheckpointStore::host_interactions_checkpoint_set_key(&checkpoint_key);
    let notification_set_key =
        RedisCheckpointStore::host_interaction_notifications_checkpoint_set_key(&checkpoint_key);
    store.receipt_transaction(
        &data_key,
        &lease_key,
        &record_key,
        &notification_key,
        &[host_set_key.as_str(), notification_set_key.as_str()],
        |connection, pipeline| {
            if let Some(raw_record) = connection
                .get::<_, Option<String>>(&record_key)
                .map_err(redis_error)?
            {
                let existing = redis_decode_host_record(&raw_record)?;
                if existing.request != request {
                    return Err(CheckpointError::new(
                        "host_interaction_conflict",
                        "interaction identity is already bound to a different request",
                    ));
                }
                let raw_checkpoint = connection
                    .get::<_, Option<String>>(&data_key)
                    .map_err(redis_error)?
                    .ok_or_else(|| {
                        CheckpointError::new(
                            "host_interaction_conflict",
                            "host interaction checkpoint is missing",
                        )
                    })?;
                let checkpoint = decode_storage(
                    &raw_checkpoint,
                    connection
                        .get::<_, Option<u64>>(&lease_key)
                        .map_err(redis_error)?,
                )?;
                let notification = connection
                    .get::<_, Option<String>>(&notification_key)
                    .map_err(redis_error)?
                    .ok_or_else(|| {
                        CheckpointError::new(
                            "host_interaction_conflict",
                            "host interaction notification is missing",
                        )
                    })?;
                let notification = redis_decode_notification(&notification)?;
                return Ok(Some(redis_host_interaction_outcome(
                    &request,
                    checkpoint.revision,
                    "replayed",
                    &existing.record_id,
                    &notification,
                )?));
            }
            let raw = connection
                .get::<_, Option<String>>(&data_key)
                .map_err(redis_error)?
                .ok_or_else(|| {
                    CheckpointError::new(
                        "host_interaction_claim_required",
                        "checkpoint does not exist",
                    )
                })?;
            let current = decode_storage(
                &raw,
                connection
                    .get::<_, Option<u64>>(&lease_key)
                    .map_err(redis_error)?,
            )?;
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
            let notification_payload = HostInteractionNotificationPayload {
                schema_version: HOST_INTERACTION_NOTIFICATION_SCHEMA.to_string(),
                notification_id: notification_id.clone(),
                record_id: record_id.clone(),
                interaction_id: request.interaction_id.clone(),
                logical_cycle: request.logical_cycle,
                status: "host_interaction".to_string(),
                wait_reason: "host_interaction".to_string(),
                prompt: redis_sanitize_public_prompt(&request.prompt),
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
            event.event_id = EventId::stable(format!("host-interaction-requested-{record_key}"))
                .map_err(|error| CheckpointError::new("event_identity_conflict", error))?;
            let mut updated = current.clone();
            updated.status = crate::checkpoint::CheckpointStatus::HostInteraction;
            updated.active_host_interaction = Some(request.clone());
            updated.claim_token = None;
            updated.claimed_cycle = None;
            updated.lease_expires_at_ms = None;
            updated.revision = current.revision + 1;
            updated
                .event_outbox
                .push(crate::runtime::state::EventOutboxEntry::pending(
                    event.event_id.as_str(),
                    serde_json::to_value(&event)?,
                )?);
            updated.validate()?;
            if updated.revision != current.revision + 1 {
                return Err(CheckpointError::new(
                    "checkpoint_revision_conflict",
                    "host interaction admission revision is invalid",
                ));
            }
            pipeline
                .set(
                    &data_key,
                    checkpoint_to_json(&updated, MAX_EXTENSION_STATE_BYTES)?,
                )
                .ignore();
            pipeline.del(&lease_key).ignore();
            pipeline
                .set(&record_key, serde_json::to_string(&record.to_value())?)
                .ignore();
            pipeline
                .set(&notification_key, redis_encode_notification(&notification)?)
                .ignore();
            pipeline.sadd(&host_set_key, &record_key).ignore();
            pipeline
                .sadd(&notification_set_key, &notification_key)
                .ignore();
            Ok(Some(redis_host_interaction_outcome(
                &request,
                updated.revision,
                "admitted",
                &record_id,
                &notification,
            )?))
        },
    )
}

fn redis_resolve_controller_command(
    store: &RedisCheckpointStore,
    command: ControllerCommand,
) -> CheckpointResult<ControllerCommandResolution> {
    command.validate()?;
    let data_key = RedisCheckpointStore::data_key(&command.handle.checkpoint_key);
    let lease_key = RedisCheckpointStore::lease_key(&command.handle.checkpoint_key);
    let receipt_key = RedisCheckpointStore::controller_command_key(&command.command_id);
    let command_key = RedisCheckpointStore::controller_command_payload_key(&command.command_id);
    let outbox_key = RedisCheckpointStore::controller_command_outbox_key(&command.command_id);
    let receipt_set_key = RedisCheckpointStore::controller_receipts_checkpoint_set_key(
        &command.handle.checkpoint_key,
    );
    let Some(current) = store.load_checkpoint(&command.handle.checkpoint_key)? else {
        return Ok(ControllerCommandResolution::Rejected {
            error: CheckpointError::new("controller_command_stale", "checkpoint does not exist")
                .to_string(),
        });
    };
    let record_key = match &command.command {
        crate::checkpoint::ControllerCommandVariant::HostInteractionResponse {
            interaction_id,
            ..
        } => Some(RedisCheckpointStore::host_interaction_key(
            &current.checkpoint_key,
            interaction_id,
        )),
        crate::checkpoint::ControllerCommandVariant::Resume => current
            .suspended_origin
            .as_ref()
            .and_then(|origin| origin.active_host_interaction.as_ref())
            .map(|request| {
                RedisCheckpointStore::host_interaction_key(
                    &current.checkpoint_key,
                    &request.interaction_id,
                )
            }),
        _ => None,
    };
    let mut watch_keys = vec![
        data_key.as_str(),
        lease_key.as_str(),
        receipt_key.as_str(),
        command_key.as_str(),
        outbox_key.as_str(),
        receipt_set_key.as_str(),
    ];
    if let Some(record_key) = record_key.as_deref() {
        watch_keys.push(record_key);
    }
    store.controller_transaction(&watch_keys, |connection, pipeline| {
        if let Some(raw_receipt) = connection
            .get::<_, Option<String>>(&receipt_key)
            .map_err(redis_error)?
        {
            let existing = redis_decode_controller_receipt(&raw_receipt)?;
            if existing.command_id != command.command_id
                || existing.command_digest != command.command_digest
            {
                return Err(CheckpointError::new(
                    "controller_command_conflict",
                    "command_id is already bound to a different command digest",
                ));
            }
            let raw_command = connection
                .get::<_, Option<String>>(&command_key)
                .map_err(redis_error)?
                .ok_or_else(|| {
                    CheckpointError::new(
                        "controller_command_conflict",
                        "controller command payload is missing",
                    )
                })?;
            let stored_command = redis_decode_controller_command(&raw_command)?;
            if stored_command.command_id != command.command_id
                || stored_command.command_digest != command.command_digest
            {
                return Err(CheckpointError::new(
                    "controller_command_conflict",
                    "controller command payload conflicts with receipt",
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
            redis_wake_outbox_matches_receipt(&wake, &existing)?;
            let Some(raw) = connection
                .get::<_, Option<String>>(&data_key)
                .map_err(redis_error)?
            else {
                return Ok(Some(ControllerCommandResolution::Rejected {
                    error: CheckpointError::new(
                        "controller_command_stale",
                        "checkpoint does not exist",
                    )
                    .to_string(),
                }));
            };
            let current = decode_storage(
                &raw,
                connection
                    .get::<_, Option<u64>>(&lease_key)
                    .map_err(redis_error)?,
            )?;
            return Ok(Some(ControllerCommandResolution::Replayed {
                receipt: existing.clone(),
                wake: redis_controller_wake(&command, &existing, &current)?,
            }));
        }
        let Some(raw) = connection
            .get::<_, Option<String>>(&data_key)
            .map_err(redis_error)?
        else {
            return Ok(Some(ControllerCommandResolution::Rejected {
                error: CheckpointError::new(
                    "controller_command_stale",
                    "checkpoint does not exist",
                )
                .to_string(),
            }));
        };
        let current = decode_storage(
            &raw,
            connection
                .get::<_, Option<u64>>(&lease_key)
                .map_err(redis_error)?,
        )?;
        let host_record = if let Some(record_key) = record_key.as_deref() {
            connection
                .get::<_, Option<String>>(record_key)
                .map_err(redis_error)?
                .as_deref()
                .map(redis_decode_host_record)
                .transpose()?
        } else {
            None
        };
        let (updated, updated_record, receipt, resolution) = match crate::runtime::stores::memory::apply_controller_command_single(
            current,
            host_record,
            &command,
        ) {
            Ok(result) => result,
            Err(error)
                if matches!(
                    error.code(),
                    "controller_command_stale" | "controller_command_terminal"
                ) => {
                    return Ok(Some(ControllerCommandResolution::Rejected {
                        error: error.to_string(),
                    }));
                }
            Err(error) => return Err(error),
        };
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
        if let (Some(record_key), Some(record)) = (record_key.as_deref(), updated_record) {
            pipeline
                .set(record_key, serde_json::to_string(&record.to_value())?)
                .ignore();
        }
        pipeline
            .set(
                &receipt_key,
                redis_encode_controller_receipt(&receipt)?,
            )
            .ignore();
        pipeline
            .set(&command_key, redis_encode_controller_command(&command)?)
            .ignore();
        let receipt = match &resolution {
            ControllerCommandResolution::Applied { receipt, .. }
            | ControllerCommandResolution::Replayed { receipt, .. } => receipt,
            ControllerCommandResolution::Rejected { .. } => {
                return Err(CheckpointError::new(
                    "controller_command_invalid_state",
                    "rejected controller command cannot be persisted",
                ))
            }
        };
        let wake = RedisControllerWakeOutbox::from_receipt(receipt)?;
        pipeline
            .set(&outbox_key, serde_json::to_string(&wake.to_value())?)
            .ignore();
        pipeline.sadd(&receipt_set_key, &receipt_key).ignore();
        Ok(Some(resolution))
    })
}

fn redis_get_controller_command_receipt(
    store: &RedisCheckpointStore,
    command_id: &str,
) -> CheckpointResult<Option<ControllerCommandReceipt>> {
    let key = RedisCheckpointStore::controller_command_key(command_id);
    let outbox_key = RedisCheckpointStore::controller_command_outbox_key(command_id);
    let mut connection = store.lock()?;
    let Some(raw) = connection.get::<_, Option<String>>(&key).map_err(redis_error)? else {
        return Ok(None);
    };
    let receipt = redis_decode_controller_receipt(&raw)?;
    let raw_command = connection
        .get::<_, Option<String>>(&RedisCheckpointStore::controller_command_payload_key(
            command_id,
        ))
        .map_err(redis_error)?
        .ok_or_else(|| {
            CheckpointError::new(
                "controller_command_conflict",
                "controller command payload is missing",
            )
        })?;
    let stored_command = redis_decode_controller_command(&raw_command)?;
    if stored_command.command_id != command_id
        || stored_command.command_digest != receipt.command_digest
    {
        return Err(CheckpointError::new(
            "controller_command_conflict",
            "controller command payload conflicts with receipt",
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
    Ok(Some(receipt))
}

fn redis_get_controller_command(
    store: &RedisCheckpointStore,
    command_id: &str,
) -> CheckpointResult<Option<ControllerCommand>> {
    let Some(receipt) = redis_get_controller_command_receipt(store, command_id)? else {
        return Ok(None);
    };
    let key = RedisCheckpointStore::controller_command_payload_key(command_id);
    let mut connection = store.lock()?;
    let Some(raw) = connection.get::<_, Option<String>>(&key).map_err(redis_error)? else {
        return Err(CheckpointError::new(
            "controller_command_conflict",
            "controller command payload is missing",
        ));
    };
    let command = redis_decode_controller_command(&raw)?;
    if command.command_id != receipt.command_id || command.command_digest != receipt.command_digest {
        return Err(CheckpointError::new(
            "controller_command_conflict",
            "controller command and receipt conflict",
        ));
    }
    Ok(Some(command))
}

fn redis_encode_controller_receipt(receipt: &ControllerCommandReceipt) -> CheckpointResult<String> {
    receipt.validate()?;
    Ok(serde_json::to_string(&receipt.to_value())?)
}

fn redis_decode_controller_receipt(raw: &str) -> CheckpointResult<ControllerCommandReceipt> {
    let value: Value = serde_json::from_str(raw).map_err(|error| {
        CheckpointError::new("controller_command_invalid_state", error.to_string())
    })?;
    ControllerCommandReceipt::from_value(&value)
}

fn redis_encode_controller_command(command: &ControllerCommand) -> CheckpointResult<String> {
    command.validate()?;
    Ok(serde_json::to_string(&command.to_value())?)
}

fn redis_decode_controller_command(raw: &str) -> CheckpointResult<ControllerCommand> {
    let value: Value = serde_json::from_str(raw).map_err(|error| {
        CheckpointError::new("controller_command_invalid_state", error.to_string())
    })?;
    ControllerCommand::from_value(&value)
}

fn redis_load_controller_command(
    connection: &mut Connection,
    command_id: &str,
    command_digest: &str,
) -> CheckpointResult<ControllerCommand> {
    let key = RedisCheckpointStore::controller_command_payload_key(command_id);
    let raw = connection
        .get::<_, Option<String>>(&key)
        .map_err(redis_error)?
        .ok_or_else(|| {
            CheckpointError::new(
                "controller_command_conflict",
                "controller command payload is missing",
            )
        })?;
    let command = redis_decode_controller_command(&raw)?;
    if command.command_id != command_id || command.command_digest != command_digest {
        return Err(CheckpointError::new(
            "controller_command_conflict",
            "controller command payload conflicts with receipt",
        ));
    }
    Ok(command)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RedisControllerWakeOutbox {
    command_id: String,
    command_digest: String,
    outbox_id: String,
    outbox_action: String,
    outbox_destination: Option<String>,
    outbox_state: String,
    attempt: u64,
    claim_token: Option<String>,
    lease_expires_at_ms: Option<u64>,
    delivered_at_ms: Option<u64>,
    last_error: Option<String>,
}

impl RedisControllerWakeOutbox {
    fn from_receipt(receipt: &ControllerCommandReceipt) -> CheckpointResult<Self> {
        let outbox = Self::from_receipt_unchecked(receipt)?;
        outbox.validate()?;
        Ok(outbox)
    }

    fn from_receipt_unchecked(receipt: &ControllerCommandReceipt) -> CheckpointResult<Self> {
        let outbox = Self {
            command_id: receipt.command_id.clone(),
            command_digest: receipt.command_digest.clone(),
            outbox_id: crate::checkpoint::controller_receipt_outbox_id(
                &receipt.command_id,
                &receipt.command_digest,
            )?,
            outbox_action: receipt.outbox_action.clone(),
            outbox_destination: receipt.outbox_destination.clone(),
            outbox_state: receipt.outbox_state.clone(),
            attempt: receipt.outbox_attempt,
            claim_token: None,
            lease_expires_at_ms: None,
            delivered_at_ms: None,
            last_error: None,
        };
        Ok(outbox)
    }

    fn validate(&self) -> CheckpointResult<()> {
        if self.command_id.trim().is_empty()
            || self.command_id.len() > crate::checkpoint::CONTROLLER_COMMAND_MAX_UTF8_BYTES
        {
            return Err(CheckpointError::new(
                "controller_command_outbox_invalid",
                "wake command_id is invalid",
            ));
        }
        crate::checkpoint::validate_sha256(&self.command_digest, "command_digest")?;
        let expected = crate::checkpoint::controller_receipt_outbox_id(
            &self.command_id,
            &self.command_digest,
        )?;
        if self.outbox_id != expected {
            return Err(CheckpointError::new(
                "controller_command_outbox_invalid",
                "wake outbox_id does not match command identity",
            ));
        }
        if !matches!(self.outbox_action.as_str(), "none" | "recovery_dispatch") {
            return Err(CheckpointError::new(
                "controller_command_outbox_invalid",
                "wake outbox_action is invalid",
            ));
        }
        if self.outbox_action == "none"
            && (self.outbox_destination.is_some()
                || self.outbox_state != "delivered"
                || self.attempt != 0)
        {
            return Err(CheckpointError::new(
                "controller_command_outbox_invalid",
                "wake none action is invalid",
            ));
        }
        if self.outbox_action == "recovery_dispatch"
            && self.outbox_destination.as_deref() != Some("distributed_advance")
        {
            return Err(CheckpointError::new(
                "controller_command_outbox_invalid",
                "wake destination is invalid",
            ));
        }
        if !matches!(
            self.outbox_state.as_str(),
            "pending" | "claimed" | "delivered" | "ambiguous"
        ) {
            return Err(CheckpointError::new(
                "controller_command_outbox_invalid",
                "wake outbox_state is invalid",
            ));
        }
        if self.attempt > crate::checkpoint::MAX_WIRE_INTEGER
            || self.lease_expires_at_ms.is_some_and(|value| value > crate::checkpoint::MAX_WIRE_INTEGER)
            || self.delivered_at_ms.is_some_and(|value| value > crate::checkpoint::MAX_WIRE_INTEGER)
        {
            return Err(CheckpointError::new(
                "controller_command_outbox_invalid",
                "wake lifecycle integer is invalid",
            ));
        }
        if self.claim_token.is_some() != self.lease_expires_at_ms.is_some() {
            return Err(CheckpointError::new(
                "controller_command_outbox_invalid",
                "wake claim and lease must be paired",
            ));
        }
        if self
            .claim_token
            .as_ref()
            .is_some_and(|value| value.trim().is_empty() || value.len() > 512)
        {
            return Err(CheckpointError::new(
                "controller_command_outbox_invalid",
                "wake claim token is invalid",
            ));
        }
        if self
            .last_error
            .as_ref()
            .is_some_and(|value| value.len() > crate::checkpoint::HOST_INTERACTION_CONTENT_MAX_UTF8_BYTES)
            || self
                .last_error
                .as_ref()
                .is_some_and(|value| crate::checkpoint::sanitize_host_text(value) != *value)
        {
            return Err(CheckpointError::new(
                "controller_command_outbox_invalid",
                "wake last_error is too large",
            ));
        }
        if self.outbox_state == "claimed" && self.claim_token.is_none() {
            return Err(CheckpointError::new(
                "controller_command_outbox_invalid",
                "claimed wake has no owner",
            ));
        }
        if self.outbox_state != "claimed" && self.claim_token.is_some() {
            return Err(CheckpointError::new(
                "controller_command_outbox_invalid",
                "unclaimed wake has an owner",
            ));
        }
        if self.outbox_action == "recovery_dispatch"
            && self.outbox_state != "pending"
            && self.attempt < 1
        {
            return Err(CheckpointError::new(
                "controller_command_outbox_invalid",
                "recovery wake requires an attempt",
            ));
        }
        Ok(())
    }

    fn to_value(&self) -> Value {
        serde_json::json!({
            "schema_version": "vv-agent.controller-command-wake.v1",
            "command_id": self.command_id,
            "command_digest": self.command_digest,
            "outbox_id": self.outbox_id,
            "outbox_action": self.outbox_action,
            "outbox_destination": self.outbox_destination,
            "outbox_state": self.outbox_state,
            "attempt": self.attempt,
            "claim_token": self.claim_token,
            "lease_expires_at_ms": self.lease_expires_at_ms,
            "delivered_at_ms": self.delivered_at_ms,
            "last_error": self.last_error,
        })
    }

    fn from_value(value: &Value) -> CheckpointResult<Self> {
        let object = value.as_object().ok_or_else(|| {
            CheckpointError::new("controller_command_outbox_invalid", "wake outbox must be an object")
        })?;
        let fields = [
            "schema_version",
            "command_id",
            "command_digest",
            "outbox_id",
            "outbox_action",
            "outbox_destination",
            "outbox_state",
            "attempt",
            "claim_token",
            "lease_expires_at_ms",
            "delivered_at_ms",
            "last_error",
        ];
        if object.len() != fields.len() || fields.iter().any(|field| !object.contains_key(*field)) {
            return Err(CheckpointError::new(
                "controller_command_outbox_invalid",
                "wake outbox has unknown or missing fields",
            ));
        }
        if object.get("schema_version").and_then(Value::as_str)
            != Some("vv-agent.controller-command-wake.v1")
        {
            return Err(CheckpointError::new(
                "controller_command_outbox_invalid",
                "unsupported wake outbox schema_version",
            ));
        }
        let nullable_string = |field: &str| -> CheckpointResult<Option<String>> {
            match object.get(field) {
                Some(Value::Null) => Ok(None),
                Some(Value::String(value)) if !value.trim().is_empty() => Ok(Some(value.clone())),
                _ => Err(CheckpointError::new(
                    "controller_command_outbox_invalid",
                    format!("wake {field} must be a string or null"),
                )),
            }
        };
        let nullable_integer = |field: &str| -> CheckpointResult<Option<u64>> {
            match object.get(field) {
                Some(Value::Null) => Ok(None),
                Some(value) => value.as_u64().map(Some).ok_or_else(|| {
                    CheckpointError::new(
                        "controller_command_outbox_invalid",
                        format!("wake {field} must be an integer or null"),
                    )
                }),
                None => Err(CheckpointError::new(
                    "controller_command_outbox_invalid",
                    format!("wake {field} is missing"),
                )),
            }
        };
        let outbox = Self {
            command_id: object
                .get("command_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            command_digest: object
                .get("command_digest")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            outbox_id: object
                .get("outbox_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            outbox_action: object
                .get("outbox_action")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            outbox_destination: nullable_string("outbox_destination")?,
            outbox_state: object
                .get("outbox_state")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            attempt: object.get("attempt").and_then(Value::as_u64).ok_or_else(|| {
                CheckpointError::new("controller_command_outbox_invalid", "wake attempt is invalid")
            })?,
            claim_token: nullable_string("claim_token")?,
            lease_expires_at_ms: nullable_integer("lease_expires_at_ms")?,
            delivered_at_ms: nullable_integer("delivered_at_ms")?,
            last_error: nullable_string("last_error")?,
        };
        outbox.validate()?;
        Ok(outbox)
    }
}
