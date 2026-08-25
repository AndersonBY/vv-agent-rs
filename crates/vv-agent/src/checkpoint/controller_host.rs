/// The passive identity carried by controller commands and receipts.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]

pub struct ControllerHandle {
    pub checkpoint_key: String,
    pub run_id: String,
    pub trace_id: String,
}

impl ControllerHandle {
    pub fn new(
        checkpoint_key: impl Into<String>,
        run_id: impl Into<String>,
        trace_id: impl Into<String>,
    ) -> CheckpointResult<Self> {
        let handle = Self {
            checkpoint_key: checkpoint_key.into(),
            run_id: run_id.into(),
            trace_id: trace_id.into(),
        };
        handle.validate()?;
        Ok(handle)
    }

    pub fn validate(&self) -> CheckpointResult<()> {
        for (name, value) in [
            ("checkpoint_key", &self.checkpoint_key),
            ("run_id", &self.run_id),
            ("trace_id", &self.trace_id),
        ] {
            if value.trim().is_empty() || value.len() > CONTROLLER_COMMAND_MAX_UTF8_BYTES {
                return Err(error(
                    "controller_command_invalid_state",
                    format!("{name} must be non-empty and at most {CONTROLLER_COMMAND_MAX_UTF8_BYTES} UTF-8 bytes"),
                ));
            }
        }
        Ok(())
    }

    pub fn to_value(&self) -> Value {
        serde_json::json!({
            "checkpoint_key": self.checkpoint_key,
            "run_id": self.run_id,
            "trace_id": self.trace_id,
        })
    }

    pub fn from_value(value: &Value) -> CheckpointResult<Self> {
        let object = value_object(value.clone(), "controller_command_invalid_state")?;
        require_exact_fields(
            &object,
            &["checkpoint_key", "run_id", "trace_id"],
            "controller_command_invalid_state",
        )?;
        Self::new(
            required_non_empty_string(
                &object,
                "checkpoint_key",
                CONTROLLER_COMMAND_MAX_UTF8_BYTES,
                "controller_command_invalid_state",
            )?,
            required_non_empty_string(
                &object,
                "run_id",
                CONTROLLER_COMMAND_MAX_UTF8_BYTES,
                "controller_command_invalid_state",
            )?,
            required_non_empty_string(
                &object,
                "trace_id",
                CONTROLLER_COMMAND_MAX_UTF8_BYTES,
                "controller_command_invalid_state",
            )?,
        )
    }
}

/// The trusted execution binding for framework-produced host interaction
/// admission.
///
/// This is deliberately separate from [`HostInteractionRequest`].  The
/// request is the language-neutral producer wire and must not carry internal
/// checkpoint or lease metadata.  A worker obtains this context from the
/// authoritative checkpoint claim and passes it to the store; the store then
/// compares every field inside the same admission CAS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostInteractionAdmissionContext {
    pub checkpoint_key: String,
    pub expected_revision: u64,
    pub claim_token: String,
    pub claimed_cycle: u64,
    pub now_ms: u64,
    pub lease_expires_at_ms: u64,
}

impl HostInteractionAdmissionContext {
    pub fn new(
        checkpoint_key: impl Into<String>,
        expected_revision: u64,
        claim_token: impl Into<String>,
        claimed_cycle: u64,
        now_ms: u64,
        lease_expires_at_ms: u64,
    ) -> CheckpointResult<Self> {
        let context = Self {
            checkpoint_key: checkpoint_key.into(),
            expected_revision,
            claim_token: claim_token.into(),
            claimed_cycle,
            now_ms,
            lease_expires_at_ms,
        };
        context.validate()?;
        Ok(context)
    }

    pub fn validate(&self) -> CheckpointResult<()> {
        if self.checkpoint_key.trim().is_empty()
            || self.checkpoint_key.len() > CONTROLLER_COMMAND_MAX_UTF8_BYTES
        {
            return Err(error(
                "host_interaction_claim_required",
                "checkpoint_key is empty or exceeds the identity limit",
            ));
        }
        if self.claim_token.trim().is_empty()
            || self.claim_token.len() > CONTROLLER_COMMAND_MAX_UTF8_BYTES
        {
            return Err(error(
                "host_interaction_claim_required",
                "claim_token is empty or exceeds the identity limit",
            ));
        }
        if self.claimed_cycle == 0 || self.claimed_cycle > MAX_WIRE_INTEGER {
            return Err(error(
                "host_interaction_claim_required",
                "claimed_cycle must be positive and JSON-safe",
            ));
        }
        if self.expected_revision > MAX_WIRE_INTEGER
            || self.now_ms > MAX_WIRE_INTEGER
            || self.lease_expires_at_ms > MAX_WIRE_INTEGER
        {
            return Err(error(
                "host_interaction_claim_required",
                "claim admission fence is outside the JSON-safe range",
            ));
        }
        if self.lease_expires_at_ms == 0 {
            return Err(error(
                "host_interaction_claim_required",
                "lease_expires_at_ms must be positive",
            ));
        }
        Ok(())
    }

    pub fn validate_live_lease(&self) -> CheckpointResult<()> {
        if self.lease_expires_at_ms <= self.now_ms {
            return Err(error(
                "host_interaction_claim_required",
                "checkpoint execution claim is stale or expired",
            ));
        }
        Ok(())
    }
}

/// A complete credential-redacted request produced by framework code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostInteractionRequest {
    pub schema_version: String,
    pub interaction_id: String,
    pub logical_cycle: u64,
    pub operation_id: String,
    pub tool_call_id: String,
    pub request_digest: String,
    pub prompt: String,
}

impl HostInteractionRequest {
    pub fn new(
        interaction_id: impl Into<String>,
        logical_cycle: u64,
        operation_id: impl Into<String>,
        tool_call_id: impl Into<String>,
        prompt: impl Into<String>,
    ) -> CheckpointResult<Self> {
        let mut request = Self {
            schema_version: HOST_INTERACTION_REQUEST_SCHEMA.to_string(),
            interaction_id: interaction_id.into(),
            logical_cycle,
            operation_id: operation_id.into(),
            tool_call_id: tool_call_id.into(),
            request_digest: String::new(),
            prompt: sanitize_host_text(&prompt.into()),
        };
        request.request_digest = request.computed_digest()?;
        request.validate()?;
        Ok(request)
    }

    pub fn validate(&self) -> CheckpointResult<()> {
        if self.schema_version != HOST_INTERACTION_REQUEST_SCHEMA {
            return Err(error(
                "host_interaction_fields_invalid",
                "unsupported host interaction request schema_version",
            ));
        }
        for (name, value) in [
            ("interaction_id", &self.interaction_id),
            ("operation_id", &self.operation_id),
            ("tool_call_id", &self.tool_call_id),
        ] {
            if value.trim().is_empty() || value.len() > HOST_INTERACTION_MAX_UTF8_BYTES {
                return Err(error(
                    "host_interaction_fields_invalid",
                    format!("{name} is empty or exceeds the identity limit"),
                ));
            }
        }
        if self.logical_cycle == 0 || self.logical_cycle > MAX_WIRE_INTEGER {
            return Err(error(
                "host_interaction_fields_invalid",
                "logical_cycle must be positive and JSON-safe",
            ));
        }
        if self.prompt.trim().is_empty() {
            return Err(error(
                "host_interaction_fields_invalid",
                "prompt must be non-empty",
            ));
        }
        if self.prompt.len() > HOST_INTERACTION_CONTENT_MAX_UTF8_BYTES {
            return Err(error(
                "host_interaction_content_too_large",
                "prompt exceeds 65536 UTF-8 bytes",
            ));
        }
        if sanitize_host_text(&self.prompt) != self.prompt {
            return Err(error(
                "host_interaction_fields_invalid",
                "prompt must be credential-redacted and contain no external locator",
            ));
        }
        validate_sha256(&self.request_digest, "request_digest").map_err(|_| {
            error(
                "host_interaction_fields_invalid",
                "request_digest must be lowercase SHA-256",
            )
        })?;
        if self.computed_digest()? != self.request_digest {
            return Err(error(
                "host_interaction_fields_invalid",
                "request_digest does not match the canonical request",
            ));
        }
        Ok(())
    }

    pub fn to_value_without_digest(&self) -> Value {
        serde_json::json!({
            "interaction_id": self.interaction_id,
            "logical_cycle": self.logical_cycle,
            "operation_id": self.operation_id,
            "prompt": self.prompt,
            "schema_version": self.schema_version,
            "tool_call_id": self.tool_call_id,
        })
    }

    pub fn computed_digest(&self) -> CheckpointResult<String> {
        canonical_digest(self.to_value(), "request_digest")
    }

    pub fn to_value(&self) -> Value {
        serde_json::json!({
            "schema_version": self.schema_version,
            "interaction_id": self.interaction_id,
            "logical_cycle": self.logical_cycle,
            "operation_id": self.operation_id,
            "tool_call_id": self.tool_call_id,
            "request_digest": self.request_digest,
            "prompt": self.prompt,
        })
    }

    pub fn from_value(value: &Value) -> CheckpointResult<Self> {
        let object = value_object(value.clone(), "host_interaction_fields_invalid")?;
        require_exact_fields(
            &object,
            &[
                "schema_version",
                "interaction_id",
                "logical_cycle",
                "operation_id",
                "tool_call_id",
                "request_digest",
                "prompt",
            ],
            "host_interaction_fields_invalid",
        )?;
        let request = Self {
            schema_version: required_string(
                &object,
                "schema_version",
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
            operation_id: required_non_empty_string(
                &object,
                "operation_id",
                HOST_INTERACTION_MAX_UTF8_BYTES,
                "host_interaction_fields_invalid",
            )?
            .to_string(),
            tool_call_id: required_non_empty_string(
                &object,
                "tool_call_id",
                HOST_INTERACTION_MAX_UTF8_BYTES,
                "host_interaction_fields_invalid",
            )?
            .to_string(),
            request_digest: required_digest(&object, "request_digest")?,
            prompt: sanitize_host_text(required_string(
                &object,
                "prompt",
                "host_interaction_fields_invalid",
            )?),
        };
        request.validate()?;
        Ok(request)
    }
}

/// The closed user message accepted by a host interaction response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostInteractionMessage {
    pub role: String,
    pub content: String,
}

impl HostInteractionMessage {
    pub fn user(content: impl Into<String>) -> CheckpointResult<Self> {
        let content = sanitize_host_text(&content.into());
        let message = Self {
            role: "user".to_string(),
            content,
        };
        message.validate()?;
        Ok(message)
    }

    pub fn validate(&self) -> CheckpointResult<()> {
        if self.role != "user" {
            return Err(error(
                "host_interaction_response_missing",
                "response.role must be user",
            ));
        }
        if self.content.trim().is_empty() {
            return Err(error(
                "host_interaction_response_missing",
                "response.content must be non-empty",
            ));
        }
        if self.content.len() > HOST_INTERACTION_CONTENT_MAX_UTF8_BYTES {
            return Err(error(
                "host_interaction_content_too_large",
                "response.content exceeds 65536 UTF-8 bytes",
            ));
        }
        Ok(())
    }

    pub fn to_value(&self) -> Value {
        serde_json::json!({"role": self.role, "content": self.content})
    }

    pub fn from_value(value: &Value) -> CheckpointResult<Self> {
        let object = value_object(value.clone(), "host_interaction_response_missing")?;
        require_exact_fields(
            &object,
            &["role", "content"],
            "host_interaction_fields_invalid",
        )?;
        let message = Self {
            role: required_string(&object, "role", "host_interaction_response_missing")?
                .to_string(),
            content: required_string(&object, "content", "host_interaction_response_missing")?
                .to_string(),
        };
        message.validate()?;
        Ok(message)
    }
}

/// The full response record stored before a recovery worker wakes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostInteractionResponse {
    pub schema_version: String,
    pub interaction_id: String,
    pub logical_cycle: u64,
    pub operation_id: String,
    pub tool_call_id: String,
    pub request_digest: String,
    pub command_id: String,
    pub response: HostInteractionMessage,
    pub response_digest: String,
}

impl HostInteractionResponse {
    pub fn new(
        interaction_id: impl Into<String>,
        logical_cycle: u64,
        operation_id: impl Into<String>,
        tool_call_id: impl Into<String>,
        request_digest: impl Into<String>,
        command_id: impl Into<String>,
        response: HostInteractionMessage,
    ) -> CheckpointResult<Self> {
        // Normalize only after validating the closed message shape.  In
        // particular, an assistant/system role must not be silently rewritten
        // as a user response while deriving the durable digest.
        response.validate()?;
        let response = HostInteractionMessage::user(response.content)?;
        let mut result = Self {
            schema_version: HOST_INTERACTION_RESPONSE_SCHEMA.to_string(),
            interaction_id: interaction_id.into(),
            logical_cycle,
            operation_id: operation_id.into(),
            tool_call_id: tool_call_id.into(),
            request_digest: request_digest.into(),
            command_id: command_id.into(),
            response,
            response_digest: String::new(),
        };
        result.response_digest = result.computed_digest()?;
        result.validate()?;
        Ok(result)
    }

    pub fn to_value_without_digest(&self) -> Value {
        serde_json::json!({
            "command_id": self.command_id,
            "interaction_id": self.interaction_id,
            "logical_cycle": self.logical_cycle,
            "operation_id": self.operation_id,
            "request_digest": self.request_digest,
            "response": self.response.to_value(),
            "schema_version": self.schema_version,
            "tool_call_id": self.tool_call_id,
        })
    }

    pub fn computed_digest(&self) -> CheckpointResult<String> {
        canonical_digest(self.to_value(), "response_digest")
    }

    pub fn to_value(&self) -> Value {
        let mut value = self.to_value_without_digest();
        value.as_object_mut().expect("response object").insert(
            "response_digest".to_string(),
            Value::String(self.response_digest.clone()),
        );
        value
    }

    pub fn validate(&self) -> CheckpointResult<()> {
        if self.schema_version != HOST_INTERACTION_RESPONSE_SCHEMA {
            return Err(error(
                "host_interaction_fields_invalid",
                "unsupported host interaction response schema_version",
            ));
        }
        for (field, value) in [
            ("interaction_id", &self.interaction_id),
            ("operation_id", &self.operation_id),
            ("tool_call_id", &self.tool_call_id),
            ("command_id", &self.command_id),
        ] {
            if value.trim().is_empty() || value.len() > HOST_INTERACTION_MAX_UTF8_BYTES {
                return Err(error(
                    "host_interaction_fields_invalid",
                    format!("{field} is empty or too long"),
                ));
            }
        }
        if self.logical_cycle == 0 || self.logical_cycle > MAX_WIRE_INTEGER {
            return Err(error(
                "host_interaction_fields_invalid",
                "logical_cycle is invalid",
            ));
        }
        validate_sha256(&self.request_digest, "request_digest").map_err(|_| {
            error(
                "host_interaction_fields_invalid",
                "request_digest is invalid",
            )
        })?;
        validate_sha256(&self.response_digest, "response_digest").map_err(|_| {
            error(
                "host_interaction_fields_invalid",
                "response_digest is invalid",
            )
        })?;
        self.response.validate()?;
        if sanitize_host_text(&self.response.content) != self.response.content {
            return Err(error(
                "host_interaction_response_missing",
                "response.content must be credential-redacted and contain no external locator",
            ));
        }
        if self.computed_digest()? != self.response_digest {
            return Err(error(
                "host_interaction_fields_invalid",
                "response_digest does not match response",
            ));
        }
        Ok(())
    }

    pub fn from_value(value: &Value) -> CheckpointResult<Self> {
        let object = value_object(value.clone(), "host_interaction_fields_invalid")?;
        require_exact_fields(
            &object,
            &[
                "schema_version",
                "interaction_id",
                "logical_cycle",
                "operation_id",
                "tool_call_id",
                "request_digest",
                "command_id",
                "response",
                "response_digest",
            ],
            "host_interaction_fields_invalid",
        )?;
        let raw_message = HostInteractionMessage::from_value(
            object.get("response").expect("exact fields checked"),
        )?;
        if sanitize_host_text(&raw_message.content) != raw_message.content {
            return Err(error(
                "host_interaction_response_missing",
                "response.content must be sanitized before decoding",
            ));
        }
        let response = Self {
            schema_version: required_string(
                &object,
                "schema_version",
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
            operation_id: required_non_empty_string(
                &object,
                "operation_id",
                HOST_INTERACTION_MAX_UTF8_BYTES,
                "host_interaction_fields_invalid",
            )?
            .to_string(),
            tool_call_id: required_non_empty_string(
                &object,
                "tool_call_id",
                HOST_INTERACTION_MAX_UTF8_BYTES,
                "host_interaction_fields_invalid",
            )?
            .to_string(),
            request_digest: required_digest(&object, "request_digest")?,
            command_id: required_non_empty_string(
                &object,
                "command_id",
                HOST_INTERACTION_MAX_UTF8_BYTES,
                "host_interaction_fields_invalid",
            )?
            .to_string(),
            response: raw_message,
            response_digest: required_digest(&object, "response_digest")?,
        };
        response.validate()?;
        Ok(response)
    }
}
pub struct HostInteractionOutcome {
    pub schema_version: String,
    pub interaction_id: String,
    pub logical_cycle: u64,
    pub checkpoint_revision: u64,
    pub status: String,
    pub outbox_state: String,
    pub record_id: String,
    pub notification_id: String,
    pub notification_payload_digest: String,
    pub notification_outbox_action: String,
    pub notification_outbox_destination: String,
}

impl HostInteractionOutcome {
    pub fn validate(&self) -> CheckpointResult<()> {
        if self.schema_version != HOST_INTERACTION_OUTCOME_SCHEMA {
            return Err(error(
                "host_interaction_fields_invalid",
                "unsupported host interaction outcome schema_version",
            ));
        }
        if self.logical_cycle == 0 || self.checkpoint_revision > MAX_WIRE_INTEGER {
            return Err(error(
                "host_interaction_fields_invalid",
                "outcome integer is invalid",
            ));
        }
        if !matches!(self.status.as_str(), "admitted" | "replayed")
            || self.outbox_state != "pending"
            || self.notification_outbox_action != "host_interaction_notification"
            || self.notification_outbox_destination != "host_interaction_observer"
        {
            return Err(error(
                "host_interaction_fields_invalid",
                "outcome status or outbox_state is invalid",
            ));
        }
        if self.interaction_id.trim().is_empty()
            || self.interaction_id.len() > HOST_INTERACTION_MAX_UTF8_BYTES
        {
            return Err(error(
                "host_interaction_fields_invalid",
                "outcome interaction_id is invalid",
            ));
        }
        for (name, value) in [
            ("record_id", &self.record_id),
            ("notification_id", &self.notification_id),
            (
                "notification_outbox_action",
                &self.notification_outbox_action,
            ),
            (
                "notification_outbox_destination",
                &self.notification_outbox_destination,
            ),
        ] {
            if value.trim().is_empty() || value.len() > CONTROLLER_COMMAND_MAX_UTF8_BYTES {
                return Err(error(
                    "host_interaction_fields_invalid",
                    format!("{name} is invalid"),
                ));
            }
        }
        validate_sha256(
            &self.notification_payload_digest,
            "notification_payload_digest",
        )
        .map_err(|_| {
            error(
                "host_interaction_fields_invalid",
                "notification_payload_digest is invalid",
            )
        })?;
        Ok(())
    }

    pub fn to_value(&self) -> Value {
        serde_json::json!({
            "schema_version": self.schema_version,
            "interaction_id": self.interaction_id,
            "logical_cycle": self.logical_cycle,
            "checkpoint_revision": self.checkpoint_revision,
            "status": self.status,
            "outbox_state": self.outbox_state,
            "record_id": self.record_id,
            "notification_id": self.notification_id,
            "notification_payload_digest": self.notification_payload_digest,
            "notification_outbox_action": self.notification_outbox_action,
            "notification_outbox_destination": self.notification_outbox_destination,
        })
    }

    pub fn from_value(value: &Value) -> CheckpointResult<Self> {
        let object = value_object(value.clone(), "host_interaction_fields_invalid")?;
        require_exact_fields(
            &object,
            &[
                "schema_version",
                "interaction_id",
                "logical_cycle",
                "checkpoint_revision",
                "status",
                "outbox_state",
                "record_id",
                "notification_id",
                "notification_payload_digest",
                "notification_outbox_action",
                "notification_outbox_destination",
            ],
            "host_interaction_fields_invalid",
        )?;
        let outcome = Self {
            schema_version: required_string(
                &object,
                "schema_version",
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
            checkpoint_revision: required_integer(&object, "checkpoint_revision", false)?,
            status: required_string(&object, "status", "host_interaction_fields_invalid")?
                .to_string(),
            outbox_state: required_string(
                &object,
                "outbox_state",
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
            notification_id: required_non_empty_string(
                &object,
                "notification_id",
                CONTROLLER_COMMAND_MAX_UTF8_BYTES,
                "host_interaction_fields_invalid",
            )?
            .to_string(),
            notification_payload_digest: required_digest(&object, "notification_payload_digest")?,
            notification_outbox_action: required_non_empty_string(
                &object,
                "notification_outbox_action",
                CONTROLLER_COMMAND_MAX_UTF8_BYTES,
                "host_interaction_fields_invalid",
            )?
            .to_string(),
            notification_outbox_destination: required_non_empty_string(
                &object,
                "notification_outbox_destination",
                CONTROLLER_COMMAND_MAX_UTF8_BYTES,
                "host_interaction_fields_invalid",
            )?
            .to_string(),
        };
        outcome.validate()?;
        Ok(outcome)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuspendedOrigin {
    pub status: String,
    pub active_host_interaction: Option<HostInteractionRequest>,
}

impl SuspendedOrigin {
    pub fn running() -> Self {
        Self {
            status: "running".to_string(),
            active_host_interaction: None,
        }
    }
    pub fn host_interaction(request: HostInteractionRequest) -> Self {
        Self {
            status: "host_interaction".to_string(),
            active_host_interaction: Some(request),
        }
    }
    pub fn validate(&self) -> CheckpointResult<()> {
        match self.status.as_str() {
            "running" if self.active_host_interaction.is_none() => Ok(()),
            "host_interaction" if self.active_host_interaction.is_some() => self
                .active_host_interaction
                .as_ref()
                .expect("checked")
                .validate(),
            _ => Err(error(
                "checkpoint_status_invalid",
                "suspended_origin status and active_host_interaction do not match",
            )),
        }
    }
    pub fn to_value(&self) -> Value {
        serde_json::json!({"status": self.status, "active_host_interaction": self.active_host_interaction.as_ref().map(HostInteractionRequest::to_value)})
    }
    pub fn from_value(value: &Value) -> CheckpointResult<Self> {
        let object = value_object(value.clone(), "checkpoint_status_invalid")?;
        require_exact_fields(
            &object,
            &["status", "active_host_interaction"],
            "checkpoint_status_invalid",
        )?;
        let origin = Self {
            status: required_string(&object, "status", "checkpoint_status_invalid")?.to_string(),
            active_host_interaction: object
                .get("active_host_interaction")
                .filter(|v| !v.is_null())
                .map(HostInteractionRequest::from_value)
                .transpose()?,
        };
        origin.validate()?;
        Ok(origin)
    }
}
