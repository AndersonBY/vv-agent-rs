#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostInteractionRecoveryEnvelope {
    pub schema_version: String,
    pub record_id: String,
    pub checkpoint_key: String,
    pub run_id: String,
    pub trace_id: String,
    pub claim_mode: String,
    pub resume_attempt: u64,
    pub expected_revision: u64,
    pub logical_cycle: u64,
    pub interaction_id: String,
    pub operation_id: String,
    pub tool_call_id: String,
    pub request_digest: String,
    pub command_id: String,
}

impl HostInteractionRecoveryEnvelope {
    pub fn validate(&self) -> CheckpointResult<()> {
        if self.schema_version != HOST_INTERACTION_RECOVERY_SCHEMA || self.claim_mode != "recovery"
        {
            return Err(error(
                "host_interaction_recovery_stale",
                "recovery envelope schema or claim_mode is invalid",
            ));
        }
        for (name, value) in [
            ("record_id", &self.record_id),
            ("checkpoint_key", &self.checkpoint_key),
            ("run_id", &self.run_id),
            ("trace_id", &self.trace_id),
            ("interaction_id", &self.interaction_id),
            ("operation_id", &self.operation_id),
            ("tool_call_id", &self.tool_call_id),
            ("command_id", &self.command_id),
        ] {
            if value.trim().is_empty() || value.len() > CONTROLLER_COMMAND_MAX_UTF8_BYTES {
                return Err(error(
                    "host_interaction_recovery_stale",
                    format!("{name} is invalid"),
                ));
            }
        }
        if self.resume_attempt == 0
            || self.resume_attempt > MAX_WIRE_INTEGER
            || self.expected_revision > MAX_WIRE_INTEGER
            || self.logical_cycle == 0
            || self.logical_cycle > MAX_WIRE_INTEGER
        {
            return Err(error(
                "host_interaction_recovery_stale",
                "recovery fence is invalid",
            ));
        }
        validate_sha256(&self.request_digest, "request_digest").map_err(|_| {
            error(
                "host_interaction_recovery_stale",
                "request_digest is invalid",
            )
        })?;
        Ok(())
    }
    pub fn to_value(&self) -> Value {
        serde_json::json!({"schema_version":self.schema_version,"record_id":self.record_id,"checkpoint_key":self.checkpoint_key,"run_id":self.run_id,"trace_id":self.trace_id,"claim_mode":self.claim_mode,"resume_attempt":self.resume_attempt,"expected_revision":self.expected_revision,"logical_cycle":self.logical_cycle,"interaction_id":self.interaction_id,"operation_id":self.operation_id,"tool_call_id":self.tool_call_id,"request_digest":self.request_digest,"command_id":self.command_id})
    }

    pub fn from_value(value: &Value) -> CheckpointResult<Self> {
        let object = value_object(value.clone(), "host_interaction_recovery_stale")?;
        require_exact_fields(
            &object,
            &[
                "schema_version",
                "record_id",
                "checkpoint_key",
                "run_id",
                "trace_id",
                "claim_mode",
                "resume_attempt",
                "expected_revision",
                "logical_cycle",
                "interaction_id",
                "operation_id",
                "tool_call_id",
                "request_digest",
                "command_id",
            ],
            "host_interaction_recovery_stale",
        )?;
        let envelope = Self {
            schema_version: required_string(
                &object,
                "schema_version",
                "host_interaction_recovery_stale",
            )?
            .to_string(),
            record_id: required_non_empty_string(
                &object,
                "record_id",
                CONTROLLER_COMMAND_MAX_UTF8_BYTES,
                "host_interaction_recovery_stale",
            )?
            .to_string(),
            checkpoint_key: required_non_empty_string(
                &object,
                "checkpoint_key",
                CONTROLLER_COMMAND_MAX_UTF8_BYTES,
                "host_interaction_recovery_stale",
            )?
            .to_string(),
            run_id: required_non_empty_string(
                &object,
                "run_id",
                CONTROLLER_COMMAND_MAX_UTF8_BYTES,
                "host_interaction_recovery_stale",
            )?
            .to_string(),
            trace_id: required_non_empty_string(
                &object,
                "trace_id",
                CONTROLLER_COMMAND_MAX_UTF8_BYTES,
                "host_interaction_recovery_stale",
            )?
            .to_string(),
            claim_mode: required_string(&object, "claim_mode", "host_interaction_recovery_stale")?
                .to_string(),
            resume_attempt: required_integer(&object, "resume_attempt", true)?,
            expected_revision: required_integer(&object, "expected_revision", false)?,
            logical_cycle: required_integer(&object, "logical_cycle", true)?,
            interaction_id: required_non_empty_string(
                &object,
                "interaction_id",
                HOST_INTERACTION_MAX_UTF8_BYTES,
                "host_interaction_recovery_stale",
            )?
            .to_string(),
            operation_id: required_non_empty_string(
                &object,
                "operation_id",
                HOST_INTERACTION_MAX_UTF8_BYTES,
                "host_interaction_recovery_stale",
            )?
            .to_string(),
            tool_call_id: required_non_empty_string(
                &object,
                "tool_call_id",
                HOST_INTERACTION_MAX_UTF8_BYTES,
                "host_interaction_recovery_stale",
            )?
            .to_string(),
            request_digest: required_digest(&object, "request_digest")?,
            command_id: required_non_empty_string(
                &object,
                "command_id",
                CONTROLLER_COMMAND_MAX_UTF8_BYTES,
                "host_interaction_recovery_stale",
            )?
            .to_string(),
        };
        envelope.validate()?;
        Ok(envelope)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostInteractionRecoveryResult {
    pub schema_version: String,
    pub kind: String,
    pub record_id: String,
    pub checkpoint_revision: Option<u64>,
    pub consumed_revision: Option<u64>,
    pub claim_mode: String,
    pub resume_attempt: Option<u64>,
    pub injection_count: u64,
    pub checkpoint_execution_claim_state: String,
    pub error: Option<String>,
}

impl HostInteractionRecoveryResult {
    pub fn validate(&self) -> CheckpointResult<()> {
        if self.schema_version != HOST_INTERACTION_RECOVERY_RESULT_SCHEMA
            || self.claim_mode != "recovery"
        {
            return Err(error(
                "host_interaction_recovery_stale",
                "recovery result schema or claim_mode is invalid",
            ));
        }
        if !matches!(self.kind.as_str(), "applied" | "replayed" | "rejected")
            || self.record_id.trim().is_empty()
        {
            return Err(error(
                "host_interaction_recovery_stale",
                "recovery result kind or record_id is invalid",
            ));
        }
        if self.injection_count > 1 {
            return Err(error(
                "host_interaction_recovery_stale",
                "injection_count must be zero or one",
            ));
        }
        if !matches!(
            self.checkpoint_execution_claim_state.as_str(),
            "retained" | "released" | "not_acquired"
        ) {
            return Err(error(
                "host_interaction_recovery_stale",
                "claim state is invalid",
            ));
        }
        if let Some(revision) = self.checkpoint_revision {
            if revision > MAX_WIRE_INTEGER {
                return Err(error(
                    "host_interaction_recovery_stale",
                    "checkpoint_revision is invalid",
                ));
            }
        }
        if let Some(revision) = self.consumed_revision {
            if revision > MAX_WIRE_INTEGER {
                return Err(error(
                    "host_interaction_recovery_stale",
                    "consumed_revision is invalid",
                ));
            }
        }
        if let Some(attempt) = self.resume_attempt {
            if attempt == 0 || attempt > MAX_WIRE_INTEGER {
                return Err(error(
                    "host_interaction_recovery_stale",
                    "resume_attempt is invalid",
                ));
            }
        }
        match self.kind.as_str() {
            "applied"
                if self.checkpoint_revision.is_some()
                    && self.consumed_revision.is_some()
                    && self.resume_attempt.is_some()
                    && self.injection_count == 1
                    && self.checkpoint_execution_claim_state == "retained"
                    && self.error.is_none() => {}
            "replayed"
                if self.checkpoint_revision.is_some()
                    && self.consumed_revision.is_some()
                    && self.resume_attempt.is_some()
                    && self.injection_count == 1
                    && self.error.is_none() => {}
            "rejected"
                if self.checkpoint_revision.is_none()
                    && self.consumed_revision.is_none()
                    && self.resume_attempt.is_none()
                    && self.injection_count == 0
                    && self.checkpoint_execution_claim_state == "not_acquired"
                    && self
                        .error
                        .as_ref()
                        .is_some_and(|value| !value.trim().is_empty()) => {}
            _ => {
                return Err(error(
                    "host_interaction_recovery_stale",
                    "recovery result fields do not match kind",
                ))
            }
        }
        Ok(())
    }
    pub fn to_value(&self) -> Value {
        let mut value = serde_json::json!({"schema_version":self.schema_version,"kind":self.kind,"record_id":self.record_id,"checkpoint_revision":self.checkpoint_revision,"consumed_revision":self.consumed_revision,"claim_mode":self.claim_mode,"resume_attempt":self.resume_attempt,"injection_count":self.injection_count,"checkpoint_execution_claim_state":self.checkpoint_execution_claim_state});
        if let Some(error) = &self.error {
            value
                .as_object_mut()
                .expect("result object")
                .insert("error".to_string(), Value::String(error.clone()));
        }
        value
    }

    pub fn from_value(value: &Value) -> CheckpointResult<Self> {
        let object = value_object(value.clone(), "host_interaction_recovery_stale")?;
        require_fields_with_optional(
            &object,
            &[
                "schema_version",
                "kind",
                "record_id",
                "checkpoint_revision",
                "consumed_revision",
                "claim_mode",
                "resume_attempt",
                "injection_count",
                "checkpoint_execution_claim_state",
            ],
            &["error"],
            "host_interaction_recovery_stale",
        )?;
        let result = Self {
            schema_version: required_string(
                &object,
                "schema_version",
                "host_interaction_recovery_stale",
            )?
            .to_string(),
            kind: required_string(&object, "kind", "host_interaction_recovery_stale")?.to_string(),
            record_id: required_non_empty_string(
                &object,
                "record_id",
                CONTROLLER_COMMAND_MAX_UTF8_BYTES,
                "host_interaction_recovery_stale",
            )?
            .to_string(),
            checkpoint_revision: optional_integer(
                &object,
                "checkpoint_revision",
                false,
                "host_interaction_recovery_stale",
            )?,
            consumed_revision: optional_integer(
                &object,
                "consumed_revision",
                false,
                "host_interaction_recovery_stale",
            )?,
            claim_mode: required_string(&object, "claim_mode", "host_interaction_recovery_stale")?
                .to_string(),
            resume_attempt: optional_integer(
                &object,
                "resume_attempt",
                true,
                "host_interaction_recovery_stale",
            )?,
            injection_count: required_integer(&object, "injection_count", false)?,
            checkpoint_execution_claim_state: required_string(
                &object,
                "checkpoint_execution_claim_state",
                "host_interaction_recovery_stale",
            )?
            .to_string(),
            error: optional_string(
                &object,
                "error",
                HOST_INTERACTION_CONTENT_MAX_UTF8_BYTES,
                "host_interaction_recovery_stale",
            )?,
        };
        result.validate()?;
        Ok(result)
    }
}

/// A small sanitized payload used by the independent UI notification outbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostInteractionNotificationPayload {
    pub schema_version: String,
    pub notification_id: String,
    pub record_id: String,
    pub interaction_id: String,
    pub logical_cycle: u64,
    pub status: String,
    pub wait_reason: String,
    pub prompt: String,
}

impl HostInteractionNotificationPayload {
    pub fn validate(&self) -> CheckpointResult<()> {
        if self.schema_version != HOST_INTERACTION_NOTIFICATION_SCHEMA
            || self.status != "host_interaction"
            || self.wait_reason != "host_interaction"
        {
            return Err(error(
                "host_interaction_fields_invalid",
                "notification payload discriminator or state is invalid",
            ));
        }
        for (name, value) in [
            ("notification_id", &self.notification_id),
            ("record_id", &self.record_id),
            ("interaction_id", &self.interaction_id),
        ] {
            if value.trim().is_empty() || value.len() > CONTROLLER_COMMAND_MAX_UTF8_BYTES {
                return Err(error(
                    "host_interaction_fields_invalid",
                    format!("{name} is invalid"),
                ));
            }
        }
        if self.logical_cycle == 0
            || self.logical_cycle > MAX_WIRE_INTEGER
            || self.prompt.trim().is_empty()
            || self.prompt.len() > HOST_INTERACTION_CONTENT_MAX_UTF8_BYTES
            || self.wait_reason.len() > CONTROLLER_COMMAND_MAX_UTF8_BYTES
        {
            return Err(error(
                "host_interaction_fields_invalid",
                "notification payload content is invalid",
            ));
        }
        if sanitize_host_text(&self.prompt) != self.prompt {
            return Err(error(
                "host_interaction_fields_invalid",
                "notification prompt is not sanitized",
            ));
        }
        if self.notification_id != notification_id_for(&self.record_id) {
            return Err(error(
                "notification_conflict",
                "notification_id does not match the canonical record identity",
            ));
        }
        Ok(())
    }
    pub fn to_value(&self) -> Value {
        serde_json::json!({"schema_version":self.schema_version,"notification_id":self.notification_id,"record_id":self.record_id,"interaction_id":self.interaction_id,"logical_cycle":self.logical_cycle,"status":self.status,"wait_reason":self.wait_reason,"prompt":self.prompt})
    }
    pub fn digest(&self) -> CheckpointResult<String> {
        let bytes = canonical_json_bytes(&self.to_value(), "host interaction notification")?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }

    pub fn from_value(value: &Value) -> CheckpointResult<Self> {
        let object = value_object(value.clone(), "host_interaction_fields_invalid")?;
        require_exact_fields(
            &object,
            &[
                "schema_version",
                "notification_id",
                "record_id",
                "interaction_id",
                "logical_cycle",
                "status",
                "wait_reason",
                "prompt",
            ],
            "host_interaction_fields_invalid",
        )?;
        let payload = Self {
            schema_version: required_string(
                &object,
                "schema_version",
                "host_interaction_fields_invalid",
            )?
            .to_string(),
            notification_id: required_non_empty_string(
                &object,
                "notification_id",
                CONTROLLER_COMMAND_MAX_UTF8_BYTES,
                "host_interaction_fields_invalid",
            )?
            .to_string(),
            record_id: required_non_empty_string(
                &object,
                "record_id",
                CONTROLLER_COMMAND_MAX_UTF8_BYTES,
                "host_interaction_fields_invalid",
            )?
            .to_string(),
            interaction_id: required_non_empty_string(
                &object,
                "interaction_id",
                HOST_INTERACTION_MAX_UTF8_BYTES,
                "host_interaction_fields_invalid",
            )?
            .to_string(),
            logical_cycle: required_integer(&object, "logical_cycle", true)?,
            status: required_string(&object, "status", "host_interaction_fields_invalid")?
                .to_string(),
            wait_reason: required_string(
                &object,
                "wait_reason",
                "host_interaction_fields_invalid",
            )?
            .to_string(),
            prompt: required_string(&object, "prompt", "host_interaction_fields_invalid")?
                .to_string(),
        };
        payload.validate()?;
        Ok(payload)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationOutboxState {
    Pending,
    Claimed,
    Delivered,
    Ambiguous,
    Aborted,
}

impl NotificationOutboxState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Claimed => "claimed",
            Self::Delivered => "delivered",
            Self::Ambiguous => "ambiguous",
            Self::Aborted => "aborted",
        }
    }
}

/// An internal representation retained by stores; its payload is always the
/// sanitized closed object above, never a raw credential-bearing request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostInteractionNotificationRecord {
    pub notification_id: String,
    pub checkpoint_key: String,
    pub record_id: String,
    pub payload: HostInteractionNotificationPayload,
    pub payload_digest: String,
    pub outbox_state: NotificationOutboxState,
    pub claim_token: Option<String>,
    pub lease_expires_at_ms: Option<u64>,
    pub attempt: u64,
    pub delivered_at_ms: Option<u64>,
    pub aborted_at_ms: Option<u64>,
    pub abort_reason: Option<String>,
    pub last_error: Option<String>,
}

impl HostInteractionNotificationRecord {
    pub fn validate(&self) -> CheckpointResult<()> {
        self.payload.validate()?;
        if self.notification_id != self.payload.notification_id
            || self.record_id != self.payload.record_id
            || self.payload_digest != self.payload.digest()?
        {
            return Err(error(
                "notification_conflict",
                "notification identity or payload digest mismatch",
            ));
        }
        validate_sha256(&self.payload_digest, "payload_digest")
            .map_err(|_| error("notification_conflict", "payload_digest is invalid"))?;
        if self.claim_token.is_some() != self.lease_expires_at_ms.is_some() {
            return Err(error(
                "notification_conflict",
                "notification claim fields must be paired",
            ));
        }
        if self.attempt > MAX_WIRE_INTEGER
            || self.lease_expires_at_ms.is_some_and(|value| value > MAX_WIRE_INTEGER)
            || self.delivered_at_ms.is_some_and(|value| value > MAX_WIRE_INTEGER)
            || self.aborted_at_ms.is_some_and(|value| value > MAX_WIRE_INTEGER)
        {
            return Err(error(
                "notification_conflict",
                "notification lifecycle integer is outside the JSON-safe range",
            ));
        }
        if self.abort_reason.as_ref().is_some_and(|value| {
            value.trim().is_empty()
                || value.len() > HOST_INTERACTION_CONTENT_MAX_UTF8_BYTES
        }) || self
            .last_error
            .as_ref()
            .is_some_and(|value| value.len() > HOST_INTERACTION_CONTENT_MAX_UTF8_BYTES)
        {
            return Err(error(
                "notification_conflict",
                "notification error text is invalid",
            ));
        }
        match self.outbox_state {
            NotificationOutboxState::Pending => {
                if self.claim_token.is_some()
                    || self.delivered_at_ms.is_some()
                    || self.aborted_at_ms.is_some()
                    || self.abort_reason.is_some()
                {
                    return Err(error(
                        "notification_conflict",
                        "pending or ambiguous notification has closed lifecycle fields",
                    ));
                }
            }
            NotificationOutboxState::Ambiguous => {
                if self.claim_token.is_some()
                    || self.delivered_at_ms.is_some()
                    || self.aborted_at_ms.is_some()
                    || self.abort_reason.is_some()
                {
                    return Err(error(
                        "notification_conflict",
                        "ambiguous notification has closed lifecycle fields",
                    ));
                }
            }
            NotificationOutboxState::Claimed => {
                if self.claim_token.is_none()
                    || self.lease_expires_at_ms.is_none()
                    || self.delivered_at_ms.is_some()
                    || self.aborted_at_ms.is_some()
                    || self.abort_reason.is_some()
                    || self.attempt == 0
                {
                    return Err(error(
                        "notification_conflict",
                        "claimed notification lifecycle fields are incomplete",
                    ));
                }
            }
            NotificationOutboxState::Delivered => {
                if self.claim_token.is_some()
                    || self.delivered_at_ms.is_none()
                    || self.aborted_at_ms.is_some()
                    || self.abort_reason.is_some()
                {
                    return Err(error(
                        "notification_conflict",
                        "delivered notification lifecycle fields are invalid",
                    ));
                }
            }
            NotificationOutboxState::Aborted => {
                if self.claim_token.is_some()
                    || self.delivered_at_ms.is_some()
                    || self.aborted_at_ms.is_none()
                    || self
                        .abort_reason
                        .as_ref()
                        .is_none_or(|value| value.trim().is_empty())
                {
                    return Err(error(
                        "notification_conflict",
                        "aborted notification lifecycle fields are invalid",
                    ));
                }
            }
        }
        Ok(())
    }
}

pub(crate) fn record_id_for(checkpoint_key: &str, request: &HostInteractionRequest) -> String {
    let value = serde_json::json!({
        "schema_version": HOST_INTERACTION_RECORD_SCHEMA,
        "checkpoint_key": checkpoint_key,
        "interaction_id": request.interaction_id,
        "logical_cycle": request.logical_cycle,
        "request_digest": request.request_digest,
    });
    let bytes = canonical_json_bytes(&value, "host interaction record identity")
        .expect("record identity is canonical JSON");
    format!("{:x}", Sha256::digest(bytes))
}
pub(crate) fn notification_id_for(record_id: &str) -> String {
    let value = serde_json::json!({
        "schema_version": HOST_INTERACTION_NOTIFICATION_SCHEMA,
        "record_id": record_id,
        "transition": "host_interaction_requested",
    });
    let bytes = canonical_json_bytes(&value, "host interaction notification identity")
        .expect("notification identity is canonical JSON");
    format!("{:x}", Sha256::digest(bytes))
}

/// Redact credential-bearing text before it enters a public notification,
/// event projection, or UI-facing outbox.
pub(crate) fn sanitize_public_text(prompt: &str) -> String {
    sanitize_host_text(prompt)
}
