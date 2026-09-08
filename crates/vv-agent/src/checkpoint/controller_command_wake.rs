#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerCommandWakeRecord {
    pub command_id: String,
    pub command_digest: String,
    pub checkpoint_key: String,
    pub handle: ControllerHandle,
    pub resume_attempt: u64,
    pub expected_revision: u64,
    pub resulting_revision: u64,
    pub resulting_status: String,
    pub outbox_id: String,
    pub outbox_state: String,
    pub outbox_action: String,
    pub outbox_destination: Option<String>,
    pub attempt: u64,
    pub claim_token: Option<String>,
    pub lease_expires_at_ms: Option<u64>,
    pub delivered_at_ms: Option<u64>,
    pub last_error: Option<String>,
}
impl ControllerCommandWakeRecord {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_receipt_lifecycle(
        receipt: &ControllerCommandReceipt,
        outbox_id: String,
        attempt: u64,
        claim_token: Option<String>,
        lease_expires_at_ms: Option<u64>,
        delivered_at_ms: Option<u64>,
        last_error: Option<String>,
    ) -> CheckpointResult<Self> {
        receipt.validate()?;
        if attempt != receipt.outbox_attempt {
            return Err(error(
                "controller_command_outbox_invalid",
                "wake record attempt conflicts with receipt",
            ));
        }
        let record = Self {
            command_id: receipt.command_id.clone(),
            command_digest: receipt.command_digest.clone(),
            checkpoint_key: receipt.handle.checkpoint_key.clone(),
            handle: receipt.handle.clone(),
            resume_attempt: receipt.resume_attempt,
            expected_revision: receipt.expected_revision,
            resulting_revision: receipt.resulting_revision,
            resulting_status: receipt.resulting_status.clone(),
            outbox_id,
            outbox_state: receipt.outbox_state.clone(),
            outbox_action: receipt.outbox_action.clone(),
            outbox_destination: receipt.outbox_destination.clone(),
            attempt,
            claim_token,
            lease_expires_at_ms,
            delivered_at_ms,
            last_error,
        };
        record.validate()?;
        Ok(record)
    }

    pub fn validate(&self) -> CheckpointResult<()> {
        if self.command_id.trim().is_empty()
            || self.command_id.len() > CONTROLLER_COMMAND_MAX_UTF8_BYTES
            || self.checkpoint_key.trim().is_empty()
            || self.checkpoint_key.len() > CONTROLLER_COMMAND_MAX_UTF8_BYTES
        {
            return Err(error(
                "controller_command_outbox_invalid",
                "wake record identity is invalid",
            ));
        }
        validate_sha256(&self.command_digest, "command_digest").map_err(|_| {
            error(
                "controller_command_outbox_invalid",
                "wake record command_digest is invalid",
            )
        })?;
        self.handle.validate()?;
        if self.handle.checkpoint_key != self.checkpoint_key
            || self.outbox_id
                != controller_receipt_outbox_id(&self.command_id, &self.command_digest)?
        {
            return Err(error(
                "controller_command_outbox_invalid",
                "wake record identity is inconsistent",
            ));
        }
        if self.resume_attempt == 0
            || self.resume_attempt > MAX_WIRE_INTEGER
            || self.expected_revision > MAX_WIRE_INTEGER
            || self.resulting_revision > MAX_WIRE_INTEGER
            || self.attempt > MAX_WIRE_INTEGER
        {
            return Err(error(
                "controller_command_outbox_invalid",
                "wake record integer is invalid",
            ));
        }
        if self.resulting_status.trim().is_empty()
            || self.resulting_status.len() > CONTROLLER_COMMAND_MAX_UTF8_BYTES
        {
            return Err(error(
                "controller_command_outbox_invalid",
                "wake record resulting_status is invalid",
            ));
        }
        if !matches!(
            self.outbox_state.as_str(),
            "pending" | "claimed" | "delivered" | "ambiguous"
        ) {
            return Err(error(
                "controller_command_outbox_invalid",
                "wake record outbox_state is invalid",
            ));
        }
        match self.outbox_action.as_str() {
            "none" if self.outbox_destination.is_none() => {
                if self.outbox_state != "delivered" || self.attempt != 0 {
                    return Err(error(
                        "controller_command_outbox_invalid",
                        "wake record none action is not delivered",
                    ));
                }
            }
            "recovery_dispatch"
                if self.outbox_destination.as_deref() == Some("distributed_advance") =>
            {
                if self.outbox_state != "pending" && self.attempt == 0 {
                    return Err(error(
                        "controller_command_outbox_invalid",
                        "wake record recovery action has no attempt",
                    ));
                }
            }
            _ => {
                return Err(error(
                    "controller_command_outbox_invalid",
                    "wake record action/destination is invalid",
                ))
            }
        }
        if self.claim_token.is_some() != self.lease_expires_at_ms.is_some() {
            return Err(error(
                "controller_command_outbox_invalid",
                "wake record claim and lease are inconsistent",
            ));
        }
        if self.outbox_state == "claimed" && self.claim_token.is_none() {
            return Err(error(
                "controller_command_outbox_invalid",
                "claimed wake record has no owner",
            ));
        }
        if self.outbox_state != "claimed" && self.claim_token.is_some() {
            return Err(error(
                "controller_command_outbox_invalid",
                "unclaimed wake record has an owner",
            ));
        }
        if self
            .claim_token
            .as_ref()
            .is_some_and(|value| {
                value.trim().is_empty() || value.len() > CONTROLLER_COMMAND_MAX_UTF8_BYTES
            })
            || self
                .lease_expires_at_ms
                .is_some_and(|value| value > MAX_WIRE_INTEGER)
            || self
                .delivered_at_ms
                .is_some_and(|value| value > MAX_WIRE_INTEGER)
        {
            return Err(error(
                "controller_command_outbox_invalid",
                "wake record lifecycle metadata is invalid",
            ));
        }
        if self.last_error.as_ref().is_some_and(|value| {
            value.len() > HOST_INTERACTION_CONTENT_MAX_UTF8_BYTES
        }) {
            return Err(error(
                "controller_command_outbox_invalid",
                "wake record last_error is invalid",
            ));
        }
        Ok(())
    }

    pub fn to_value(&self) -> Value {
        serde_json::json!({
            "command_id": self.command_id,
            "command_digest": self.command_digest,
            "checkpoint_key": self.checkpoint_key,
            "handle": self.handle.to_value(),
            "resume_attempt": self.resume_attempt,
            "expected_revision": self.expected_revision,
            "resulting_revision": self.resulting_revision,
            "resulting_status": self.resulting_status,
            "outbox_id": self.outbox_id,
            "outbox_state": self.outbox_state,
            "outbox_action": self.outbox_action,
            "outbox_destination": self.outbox_destination,
            "attempt": self.attempt,
            "claim_token": self.claim_token,
            "lease_expires_at_ms": self.lease_expires_at_ms,
            "delivered_at_ms": self.delivered_at_ms,
            "last_error": self.last_error,
        })
    }

    pub fn from_value(value: &Value) -> CheckpointResult<Self> {
        let object = value_object(value.clone(), "controller_command_outbox_invalid")?;
        require_exact_fields(
            &object,
            &[
                "command_id",
                "command_digest",
                "checkpoint_key",
                "handle",
                "resume_attempt",
                "expected_revision",
                "resulting_revision",
                "resulting_status",
                "outbox_id",
                "outbox_state",
                "outbox_action",
                "outbox_destination",
                "attempt",
                "claim_token",
                "lease_expires_at_ms",
                "delivered_at_ms",
                "last_error",
            ],
            "controller_command_outbox_invalid",
        )?;
        let record = Self {
            command_id: required_non_empty_string(
                &object,
                "command_id",
                CONTROLLER_COMMAND_MAX_UTF8_BYTES,
                "controller_command_outbox_invalid",
            )?
            .to_string(),
            command_digest: required_digest(&object, "command_digest")?,
            checkpoint_key: required_non_empty_string(
                &object,
                "checkpoint_key",
                CONTROLLER_COMMAND_MAX_UTF8_BYTES,
                "controller_command_outbox_invalid",
            )?
            .to_string(),
            handle: ControllerHandle::from_value(
                object.get("handle").expect("exact fields checked"),
            )?,
            resume_attempt: required_integer(&object, "resume_attempt", true)?,
            expected_revision: required_integer(&object, "expected_revision", false)?,
            resulting_revision: required_integer(&object, "resulting_revision", false)?,
            resulting_status: required_string(
                &object,
                "resulting_status",
                "controller_command_outbox_invalid",
            )?
            .to_string(),
            outbox_id: required_non_empty_string(
                &object,
                "outbox_id",
                CONTROLLER_COMMAND_MAX_UTF8_BYTES,
                "controller_command_outbox_invalid",
            )?
            .to_string(),
            outbox_state: required_string(
                &object,
                "outbox_state",
                "controller_command_outbox_invalid",
            )?
            .to_string(),
            outbox_action: required_string(
                &object,
                "outbox_action",
                "controller_command_outbox_invalid",
            )?
            .to_string(),
            outbox_destination: optional_string(
                &object,
                "outbox_destination",
                CONTROLLER_COMMAND_MAX_UTF8_BYTES,
                "controller_command_outbox_invalid",
            )?,
            attempt: required_integer(&object, "attempt", false)?,
            claim_token: optional_string(
                &object,
                "claim_token",
                CONTROLLER_COMMAND_MAX_UTF8_BYTES,
                "controller_command_outbox_invalid",
            )?,
            lease_expires_at_ms: optional_integer(
                &object,
                "lease_expires_at_ms",
                false,
                "controller_command_outbox_invalid",
            )?,
            delivered_at_ms: optional_integer(
                &object,
                "delivered_at_ms",
                false,
                "controller_command_outbox_invalid",
            )?,
            last_error: optional_string(
                &object,
                "last_error",
                HOST_INTERACTION_CONTENT_MAX_UTF8_BYTES,
                "controller_command_outbox_invalid",
            )?,
        };
        record.validate()?;
        Ok(record)
    }
}
