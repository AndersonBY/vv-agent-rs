#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostInteractionRecord {
    pub schema_version: String,
    pub record_id: String,
    pub checkpoint_key: String,
    pub interaction_id: String,
    pub logical_cycle: u64,
    pub attempt: u64,
    pub claim_token: Option<String>,
    pub lease_expires_at_ms: Option<u64>,
    pub request: HostInteractionRequest,
    pub request_digest: String,
    pub state: String,
    pub response: Option<HostInteractionResponse>,
    pub response_digest: Option<String>,
    pub command_id: Option<String>,
    pub resolved_revision: Option<u64>,
    pub consumed_revision: Option<u64>,
    pub last_error: Option<String>,
}

impl HostInteractionRecord {
    pub fn validate(&self) -> CheckpointResult<()> {
        if self.schema_version != HOST_INTERACTION_RECORD_SCHEMA {
            return Err(error(
                "host_interaction_fields_invalid",
                "unsupported host interaction record schema_version",
            ));
        }
        if self.record_id.trim().is_empty()
            || self.record_id.len() > CONTROLLER_COMMAND_MAX_UTF8_BYTES
            || self.checkpoint_key.trim().is_empty()
            || self.checkpoint_key.len() > CONTROLLER_COMMAND_MAX_UTF8_BYTES
        {
            return Err(error(
                "host_interaction_fields_invalid",
                "record identity is invalid",
            ));
        }
        if self.logical_cycle == 0 || self.attempt > MAX_WIRE_INTEGER {
            return Err(error(
                "host_interaction_fields_invalid",
                "record integer is invalid",
            ));
        }
        self.request.validate()?;
        if self.record_id != record_id_for(&self.checkpoint_key, &self.request) {
            return Err(error(
                "host_interaction_fields_invalid",
                "record_id does not match the canonical request identity",
            ));
        }
        if self.request.interaction_id != self.interaction_id
            || self.request.logical_cycle != self.logical_cycle
            || self.request_digest != self.request.request_digest
        {
            return Err(error(
                "host_interaction_fields_invalid",
                "record request identity does not match request",
            ));
        }
        validate_sha256(&self.request_digest, "request_digest").map_err(|_| {
            error(
                "host_interaction_fields_invalid",
                "record request_digest is invalid",
            )
        })?;
        match self.state.as_str() {
            "active" => {
                if self.response.is_some()
                    || self.response_digest.is_some()
                    || self.command_id.is_some()
                    || self.resolved_revision.is_some()
                    || self.consumed_revision.is_some()
                    || self.claim_token.is_some()
                    || self.lease_expires_at_ms.is_some()
                {
                    return Err(error(
                        "host_interaction_fields_invalid",
                        "active record carries resolved state",
                    ));
                }
            }
            "resolved_pending" => {
                if self.response.is_none()
                    || self.response_digest.is_none()
                    || self.command_id.is_none()
                    || self.resolved_revision.is_none()
                    || self.consumed_revision.is_some()
                    || self.claim_token.is_some()
                    || self.lease_expires_at_ms.is_some()
                {
                    return Err(error(
                        "host_interaction_fields_invalid",
                        "resolved_pending record state is incomplete",
                    ));
                }
            }
            "resolved_claimed" => {
                if self.response.is_none()
                    || self.response_digest.is_none()
                    || self.command_id.is_none()
                    || self.resolved_revision.is_none()
                    || self.consumed_revision.is_some()
                    || self.claim_token.is_none()
                    || self.lease_expires_at_ms.is_none()
                {
                    return Err(error(
                        "host_interaction_fields_invalid",
                        "resolved_claimed record state is incomplete",
                    ));
                }
            }
            "consumed" => {
                if self.response.is_none()
                    || self.response_digest.is_none()
                    || self.command_id.is_none()
                    || self.resolved_revision.is_none()
                    || self.consumed_revision.is_none()
                    || self.claim_token.is_some()
                    || self.lease_expires_at_ms.is_some()
                {
                    return Err(error(
                        "host_interaction_fields_invalid",
                        "consumed record state is incomplete",
                    ));
                }
            }
            _ => {
                return Err(error(
                    "host_interaction_fields_invalid",
                    "unsupported host interaction record state",
                ))
            }
        }
        if let Some(response) = &self.response {
            response.validate()?;
            if response.interaction_id != self.interaction_id
                || response.logical_cycle != self.logical_cycle
                || response.request_digest != self.request_digest
                || self.response_digest.as_deref() != Some(response.response_digest.as_str())
                || self.command_id.as_deref() != Some(response.command_id.as_str())
            {
                return Err(error(
                    "host_interaction_fields_invalid",
                    "record response identity does not match response",
                ));
            }
        }
        if self.claim_token.as_deref().is_some_and(str::is_empty)
            || self
                .lease_expires_at_ms
                .is_some_and(|v| v > MAX_WIRE_INTEGER)
        {
            return Err(error(
                "host_interaction_fields_invalid",
                "record claim is invalid",
            ));
        }
        if self.claim_token.is_some() != self.lease_expires_at_ms.is_some() {
            return Err(error(
                "host_interaction_fields_invalid",
                "record claim fields must be paired",
            ));
        }
        Ok(())
    }
    pub fn to_value(&self) -> Value {
        let mut value = serde_json::json!({
            "schema_version": self.schema_version,
            "record_id": self.record_id,
            "checkpoint_key": self.checkpoint_key,
            "interaction_id": self.interaction_id,
            "logical_cycle": self.logical_cycle,
            "request": self.request.to_value(),
            "request_digest": self.request_digest,
            "state": self.state,
            "attempt": self.attempt,
            "claim_token": self.claim_token,
            "lease_expires_at_ms": self.lease_expires_at_ms,
            "response": self.response.as_ref().map(HostInteractionResponse::to_value),
            "response_digest": self.response_digest,
            "command_id": self.command_id,
            "resolved_revision": self.resolved_revision,
            "consumed_revision": self.consumed_revision,
        });
        if let Some(last_error) = &self.last_error {
            value
                .as_object_mut()
                .expect("record object")
                .insert("last_error".to_string(), Value::String(last_error.clone()));
        }
        value
    }

    pub fn from_value(value: &Value) -> CheckpointResult<Self> {
        let object = value_object(value.clone(), "host_interaction_fields_invalid")?;
        require_fields_with_optional(
            &object,
            &[
                "schema_version",
                "record_id",
                "checkpoint_key",
                "interaction_id",
                "logical_cycle",
                "request",
                "request_digest",
                "state",
                "attempt",
                "claim_token",
                "lease_expires_at_ms",
                "response",
                "response_digest",
                "command_id",
                "resolved_revision",
                "consumed_revision",
            ],
            &["last_error"],
            "host_interaction_fields_invalid",
        )?;
        let record = Self {
            schema_version: required_string(
                &object,
                "schema_version",
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
            checkpoint_key: required_non_empty_string(
                &object,
                "checkpoint_key",
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
            attempt: required_integer(&object, "attempt", false)?,
            claim_token: optional_string(
                &object,
                "claim_token",
                CONTROLLER_COMMAND_MAX_UTF8_BYTES,
                "host_interaction_fields_invalid",
            )?,
            lease_expires_at_ms: optional_integer(
                &object,
                "lease_expires_at_ms",
                false,
                "host_interaction_fields_invalid",
            )?,
            request: HostInteractionRequest::from_value(
                object.get("request").expect("required fields checked"),
            )?,
            request_digest: required_digest(&object, "request_digest")?,
            state: required_string(&object, "state", "host_interaction_fields_invalid")?
                .to_string(),
            response: object
                .get("response")
                .filter(|value| !value.is_null())
                .map(HostInteractionResponse::from_value)
                .transpose()?,
            response_digest: optional_string(
                &object,
                "response_digest",
                64,
                "host_interaction_fields_invalid",
            )?,
            command_id: optional_string(
                &object,
                "command_id",
                CONTROLLER_COMMAND_MAX_UTF8_BYTES,
                "host_interaction_fields_invalid",
            )?,
            resolved_revision: optional_integer(
                &object,
                "resolved_revision",
                false,
                "host_interaction_fields_invalid",
            )?,
            consumed_revision: optional_integer(
                &object,
                "consumed_revision",
                false,
                "host_interaction_fields_invalid",
            )?,
            last_error: optional_string(
                &object,
                "last_error",
                HOST_INTERACTION_CONTENT_MAX_UTF8_BYTES,
                "host_interaction_fields_invalid",
            )?,
        };
        record.validate()?;
        Ok(record)
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControllerCommandVariant {
    HostInteractionResponse {
        interaction_id: String,
        logical_cycle: u64,
        operation_id: String,
        tool_call_id: String,
        request_digest: String,
        response: HostInteractionMessage,
    },
    Suspend,
    Resume,
    Cancel,
    Abort,
}

pub type ControllerCommandKind = ControllerCommandVariant;

impl ControllerCommandVariant {
    fn to_value(&self) -> Value {
        match self {
            Self::HostInteractionResponse {
                interaction_id,
                logical_cycle,
                operation_id,
                tool_call_id,
                request_digest,
                response,
            } => {
                serde_json::json!({"kind":"host_interaction_response","interaction_id":interaction_id,"logical_cycle":logical_cycle,"operation_id":operation_id,"tool_call_id":tool_call_id,"request_digest":request_digest,"response":response.to_value()})
            }
            Self::Suspend => serde_json::json!({"kind":"suspend"}),
            Self::Resume => serde_json::json!({"kind":"resume"}),
            Self::Cancel => serde_json::json!({"kind":"cancel"}),
            Self::Abort => serde_json::json!({"kind":"abort"}),
        }
    }
    fn from_value(value: &Value) -> CheckpointResult<Self> {
        let object = value_object(value.clone(), "controller_command_invalid_state")?;
        let kind = required_string(&object, "kind", "controller_command_invalid_state")?;
        match kind {
            "host_interaction_response" => {
                require_exact_fields(
                    &object,
                    &[
                        "kind",
                        "interaction_id",
                        "logical_cycle",
                        "operation_id",
                        "tool_call_id",
                        "request_digest",
                        "response",
                    ],
                    "controller_command_invalid_state",
                )?;
                let raw_response = HostInteractionMessage::from_value(
                    object.get("response").expect("exact fields checked"),
                )?;
                // Python's canonical command reader normalizes the response
                // before deriving the command digest.  Do the same at the
                // Rust wire boundary so a transport-supplied credential or
                // external locator can never cross the controller CAS.
                let response = HostInteractionMessage::user(raw_response.content)?;
                Ok(Self::HostInteractionResponse {
                    interaction_id: required_non_empty_string(
                        &object,
                        "interaction_id",
                        HOST_INTERACTION_MAX_UTF8_BYTES,
                        "controller_command_invalid_state",
                    )?
                    .to_string(),
                    logical_cycle: required_integer(&object, "logical_cycle", true)?,
                    operation_id: required_non_empty_string(
                        &object,
                        "operation_id",
                        HOST_INTERACTION_MAX_UTF8_BYTES,
                        "controller_command_invalid_state",
                    )?
                    .to_string(),
                    tool_call_id: required_non_empty_string(
                        &object,
                        "tool_call_id",
                        HOST_INTERACTION_MAX_UTF8_BYTES,
                        "controller_command_invalid_state",
                    )?
                    .to_string(),
                    request_digest: required_digest(&object, "request_digest")?,
                    response,
                })
            }
            "suspend" => {
                require_exact_fields(&object, &["kind"], "controller_command_invalid_state")?;
                Ok(Self::Suspend)
            }
            "resume" => {
                require_exact_fields(&object, &["kind"], "controller_command_invalid_state")?;
                Ok(Self::Resume)
            }
            "cancel" => {
                require_exact_fields(&object, &["kind"], "controller_command_invalid_state")?;
                Ok(Self::Cancel)
            }
            "abort" => {
                require_exact_fields(&object, &["kind"], "controller_command_invalid_state")?;
                Ok(Self::Abort)
            }
            _ => Err(error(
                "controller_command_invalid_state",
                "unsupported controller command kind",
            )),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerCommand {
    pub schema_version: String,
    pub command_id: String,
    pub command_digest: String,
    pub handle: ControllerHandle,
    pub resume_attempt: u64,
    pub expected_revision: u64,
    pub command: ControllerCommandVariant,
}

impl ControllerCommand {
    pub fn new(
        command_id: impl Into<String>,
        handle: ControllerHandle,
        resume_attempt: u64,
        expected_revision: u64,
        command: ControllerCommandVariant,
    ) -> CheckpointResult<Self> {
        let mut result = Self {
            schema_version: CONTROLLER_COMMAND_SCHEMA.to_string(),
            command_id: command_id.into(),
            command_digest: String::new(),
            handle,
            resume_attempt,
            expected_revision,
            command,
        };
        result.command_digest = result.computed_digest()?;
        result.validate()?;
        Ok(result)
    }
    pub fn to_value_without_digest(&self) -> Value {
        serde_json::json!({"command":self.command.to_value(),"expected_revision":self.expected_revision,"handle":self.handle.to_value(),"resume_attempt":self.resume_attempt,"schema_version":self.schema_version,"command_id":self.command_id})
    }
    pub fn computed_digest(&self) -> CheckpointResult<String> {
        canonical_digest(self.to_value(), "command_digest")
    }
    pub fn to_value(&self) -> Value {
        let mut value = self.to_value_without_digest();
        value.as_object_mut().expect("command object").insert(
            "command_digest".to_string(),
            Value::String(self.command_digest.clone()),
        );
        value
    }
    pub fn validate(&self) -> CheckpointResult<()> {
        if self.schema_version != CONTROLLER_COMMAND_SCHEMA {
            return Err(error(
                "controller_command_digest_invalid",
                "unsupported controller command schema_version",
            ));
        }
        if self.command_id.trim().is_empty()
            || self.command_id.len() > CONTROLLER_COMMAND_MAX_UTF8_BYTES
        {
            return Err(error(
                "controller_command_digest_invalid",
                "command_id is invalid",
            ));
        }
        self.handle.validate()?;
        if self.resume_attempt == 0
            || self.resume_attempt > MAX_WIRE_INTEGER
            || self.expected_revision > MAX_WIRE_INTEGER
        {
            return Err(error(
                "controller_command_digest_invalid",
                "controller fence is invalid",
            ));
        }
        if let ControllerCommandVariant::HostInteractionResponse {
            interaction_id,
            logical_cycle,
            operation_id,
            tool_call_id,
            request_digest,
            response,
        } = &self.command
        {
            for value in [interaction_id, operation_id, tool_call_id] {
                if value.trim().is_empty() || value.len() > HOST_INTERACTION_MAX_UTF8_BYTES {
                    return Err(error(
                        "controller_command_invalid_state",
                        "host response identity is invalid",
                    ));
                }
            }
            if *logical_cycle == 0 || *logical_cycle > MAX_WIRE_INTEGER {
                return Err(error(
                    "controller_command_invalid_state",
                    "host response logical_cycle is invalid",
                ));
            }
            validate_sha256(request_digest, "request_digest").map_err(|_| {
                error(
                    "controller_command_invalid_state",
                    "request_digest is invalid",
                )
            })?;
            response.validate()?;
        }
        validate_sha256(&self.command_digest, "command_digest").map_err(|_| {
            error(
                "controller_command_digest_invalid",
                "command_digest is invalid",
            )
        })?;
        if self.computed_digest()? != self.command_digest {
            return Err(error(
                "controller_command_digest_invalid",
                "command_digest does not match the complete request",
            ));
        }
        Ok(())
    }
    pub fn from_value(value: &Value) -> CheckpointResult<Self> {
        let object = value_object(value.clone(), "controller_command_digest_invalid")?;
        require_exact_fields(
            &object,
            &[
                "schema_version",
                "command_id",
                "command_digest",
                "handle",
                "resume_attempt",
                "expected_revision",
                "command",
            ],
            "controller_command_digest_invalid",
        )?;
        let command = Self {
            schema_version: required_string(
                &object,
                "schema_version",
                "controller_command_digest_invalid",
            )?
            .to_string(),
            command_id: required_non_empty_string(
                &object,
                "command_id",
                CONTROLLER_COMMAND_MAX_UTF8_BYTES,
                "controller_command_digest_invalid",
            )?
            .to_string(),
            command_digest: required_digest(&object, "command_digest")?,
            handle: ControllerHandle::from_value(
                object.get("handle").expect("exact fields checked"),
            )?,
            resume_attempt: required_integer(&object, "resume_attempt", true)?,
            expected_revision: required_integer(&object, "expected_revision", false)?,
            command: ControllerCommandVariant::from_value(
                object.get("command").expect("exact fields checked"),
            )?,
        };
        command.validate()?;
        Ok(command)
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerCommandReceipt {
    pub schema_version: String,
    pub command_id: String,
    pub command_digest: String,
    pub handle: ControllerHandle,
    pub resume_attempt: u64,
    pub expected_revision: u64,
    pub resulting_revision: u64,
    pub resulting_status: String,
    pub outbox_state: String,
    pub outbox_action: String,
    pub outbox_destination: Option<String>,
    pub outbox_attempt: u64,
}

impl ControllerCommandReceipt {
    pub fn validate(&self) -> CheckpointResult<()> {
        if self.schema_version != CONTROLLER_COMMAND_RECEIPT_SCHEMA {
            return Err(error(
                "controller_command_invalid_state",
                "unsupported receipt schema_version",
            ));
        }
        if self.command_id.trim().is_empty()
            || self.command_id.len() > CONTROLLER_COMMAND_MAX_UTF8_BYTES
        {
            return Err(error(
                "controller_command_invalid_state",
                "receipt command_id is invalid",
            ));
        }
        validate_sha256(&self.command_digest, "command_digest").map_err(|_| {
            error(
                "controller_command_invalid_state",
                "receipt command_digest is invalid",
            )
        })?;
        self.handle.validate()?;
        if self.resume_attempt == 0
            || self.resume_attempt > MAX_WIRE_INTEGER
            || self.expected_revision > MAX_WIRE_INTEGER
            || self.resulting_revision > MAX_WIRE_INTEGER
            || self.outbox_attempt > MAX_WIRE_INTEGER
        {
            return Err(error(
                "controller_command_invalid_state",
                "receipt integer is invalid",
            ));
        }
        if !matches!(
            self.outbox_state.as_str(),
            "pending" | "claimed" | "delivered" | "ambiguous"
        ) {
            return Err(error(
                "controller_command_invalid_state",
                "receipt outbox_state is invalid",
            ));
        }
        if self.resulting_status.trim().is_empty()
            || self.resulting_status.len() > CONTROLLER_COMMAND_MAX_UTF8_BYTES
        {
            return Err(error(
                "controller_command_invalid_state",
                "receipt resulting_status is invalid",
            ));
        }
        if self
            .outbox_destination
            .as_ref()
            .is_some_and(|value| value.len() > CONTROLLER_COMMAND_MAX_UTF8_BYTES)
        {
            return Err(error(
                "controller_command_invalid_state",
                "receipt outbox_destination is too long",
            ));
        }
        match self.outbox_action.as_str() {
            "none" if self.outbox_destination.is_none() => {}
            "recovery_dispatch"
                if self.outbox_destination.as_deref() == Some("distributed_advance") => {}
            _ => {
                return Err(error(
                    "controller_command_invalid_state",
                    "outbox action/destination is not the canonical recovery pair",
                ))
            }
        }
        if self.outbox_action == "none" && self.outbox_state != "delivered" {
            return Err(error(
                "controller_command_invalid_state",
                "a non-waking receipt must be delivered",
            ));
        }
        Ok(())
    }
    pub fn to_value(&self) -> Value {
        serde_json::json!({"schema_version":self.schema_version,"command_id":self.command_id,"command_digest":self.command_digest,"handle":self.handle.to_value(),"resume_attempt":self.resume_attempt,"expected_revision":self.expected_revision,"resulting_revision":self.resulting_revision,"resulting_status":self.resulting_status,"outbox_state":self.outbox_state,"outbox_action":self.outbox_action,"outbox_destination":self.outbox_destination,"outbox_attempt":self.outbox_attempt})
    }

    pub fn from_value(value: &Value) -> CheckpointResult<Self> {
        let object = value_object(value.clone(), "controller_command_invalid_state")?;
        require_exact_fields(
            &object,
            &[
                "schema_version",
                "command_id",
                "command_digest",
                "handle",
                "resume_attempt",
                "expected_revision",
                "resulting_revision",
                "resulting_status",
                "outbox_state",
                "outbox_action",
                "outbox_destination",
                "outbox_attempt",
            ],
            "controller_command_invalid_state",
        )?;
        let receipt = Self {
            schema_version: required_string(
                &object,
                "schema_version",
                "controller_command_invalid_state",
            )?
            .to_string(),
            command_id: required_non_empty_string(
                &object,
                "command_id",
                CONTROLLER_COMMAND_MAX_UTF8_BYTES,
                "controller_command_invalid_state",
            )?
            .to_string(),
            command_digest: required_digest(&object, "command_digest")?,
            handle: ControllerHandle::from_value(
                object.get("handle").expect("required fields checked"),
            )?,
            resume_attempt: required_integer(&object, "resume_attempt", true)?,
            expected_revision: required_integer(&object, "expected_revision", false)?,
            resulting_revision: required_integer(&object, "resulting_revision", false)?,
            resulting_status: required_string(
                &object,
                "resulting_status",
                "controller_command_invalid_state",
            )?
            .to_string(),
            outbox_state: required_string(
                &object,
                "outbox_state",
                "controller_command_invalid_state",
            )?
            .to_string(),
            outbox_action: required_string(
                &object,
                "outbox_action",
                "controller_command_invalid_state",
            )?
            .to_string(),
            outbox_destination: optional_string(
                &object,
                "outbox_destination",
                CONTROLLER_COMMAND_MAX_UTF8_BYTES,
                "controller_command_invalid_state",
            )?,
            outbox_attempt: required_integer(&object, "outbox_attempt", false)?,
        };
        receipt.validate()?;
        Ok(receipt)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerCommandWake {
    pub action: String,
    pub destination: Option<String>,
    pub logical_cycle: u64,
    pub claim_mode: String,
}

impl ControllerCommandWake {
    pub fn none() -> Self {
        Self {
            action: "none".to_string(),
            destination: None,
            logical_cycle: 0,
            claim_mode: "none".to_string(),
        }
    }
    pub fn recovery(logical_cycle: u64) -> Self {
        Self {
            action: "recovery_dispatch".to_string(),
            destination: Some("distributed_advance".to_string()),
            logical_cycle,
            claim_mode: "recovery".to_string(),
        }
    }
    pub fn validate(&self) -> CheckpointResult<()> {
        match self.action.as_str() {
            "none" if self.destination.is_none() && self.claim_mode == "none" => Ok(()),
            "recovery_dispatch"
                if self.destination.as_deref() == Some("distributed_advance")
                    && self.claim_mode == "recovery"
                    && self.logical_cycle > 0 =>
            {
                Ok(())
            }
            _ => Err(error("controller_command_invalid_state", "wake is invalid")),
        }
    }
    pub fn to_value(&self) -> Value {
        serde_json::json!({"action":self.action,"destination":self.destination,"logical_cycle":self.logical_cycle,"claim_mode":self.claim_mode})
    }

    pub fn from_value(value: &Value) -> CheckpointResult<Self> {
        let object = value_object(value.clone(), "controller_command_invalid_state")?;
        require_exact_fields(
            &object,
            &["action", "destination", "logical_cycle", "claim_mode"],
            "controller_command_invalid_state",
        )?;
        let wake = Self {
            action: required_string(&object, "action", "controller_command_invalid_state")?
                .to_string(),
            destination: optional_string(
                &object,
                "destination",
                CONTROLLER_COMMAND_MAX_UTF8_BYTES,
                "controller_command_invalid_state",
            )?,
            logical_cycle: required_integer(&object, "logical_cycle", false)?,
            claim_mode: required_string(&object, "claim_mode", "controller_command_invalid_state")?
                .to_string(),
        };
        wake.validate()?;
        Ok(wake)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControllerCommandResolution {
    Applied {
        receipt: ControllerCommandReceipt,
        wake: ControllerCommandWake,
    },
    Replayed {
        receipt: ControllerCommandReceipt,
        wake: ControllerCommandWake,
    },
    Rejected {
        error: String,
    },
}

impl ControllerCommandResolution {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Applied { .. } => "applied",
            Self::Replayed { .. } => "replayed",
            Self::Rejected { .. } => "rejected",
        }
    }
    pub fn to_value(&self) -> Value {
        match self {
            Self::Applied { receipt, wake } => {
                serde_json::json!({"schema_version":CONTROLLER_COMMAND_RESOLUTION_SCHEMA,"kind":"applied","receipt":receipt.to_value(),"wake":wake.to_value()})
            }
            Self::Replayed { receipt, wake } => {
                serde_json::json!({"schema_version":CONTROLLER_COMMAND_RESOLUTION_SCHEMA,"kind":"replayed","receipt":receipt.to_value(),"wake":wake.to_value()})
            }
            Self::Rejected { error } => {
                serde_json::json!({"schema_version":CONTROLLER_COMMAND_RESOLUTION_SCHEMA,"kind":"rejected","error":error})
            }
        }
    }

    pub fn from_value(value: &Value) -> CheckpointResult<Self> {
        let object = value_object(value.clone(), "controller_command_invalid_state")?;
        let kind = required_string(&object, "kind", "controller_command_invalid_state")?;
        match kind {
            "applied" | "replayed" => {
                require_exact_fields(
                    &object,
                    &["schema_version", "kind", "receipt", "wake"],
                    "controller_command_invalid_state",
                )?;
                if required_string(
                    &object,
                    "schema_version",
                    "controller_command_invalid_state",
                )? != CONTROLLER_COMMAND_RESOLUTION_SCHEMA
                {
                    return Err(error(
                        "controller_command_invalid_state",
                        "unsupported resolution schema_version",
                    ));
                }
                let receipt = ControllerCommandReceipt::from_value(
                    object.get("receipt").expect("required fields checked"),
                )?;
                let wake = ControllerCommandWake::from_value(
                    object.get("wake").expect("required fields checked"),
                )?;
                if kind == "applied" {
                    Ok(Self::Applied { receipt, wake })
                } else {
                    Ok(Self::Replayed { receipt, wake })
                }
            }
            "rejected" => {
                require_exact_fields(
                    &object,
                    &["schema_version", "kind", "error"],
                    "controller_command_invalid_state",
                )?;
                if required_string(
                    &object,
                    "schema_version",
                    "controller_command_invalid_state",
                )? != CONTROLLER_COMMAND_RESOLUTION_SCHEMA
                {
                    return Err(error(
                        "controller_command_invalid_state",
                        "unsupported resolution schema_version",
                    ));
                }
                let message = required_non_empty_string(
                    &object,
                    "error",
                    HOST_INTERACTION_CONTENT_MAX_UTF8_BYTES,
                    "controller_command_invalid_state",
                )?;
                Ok(Self::Rejected {
                    error: message.to_string(),
                })
            }
            _ => Err(error(
                "controller_command_invalid_state",
                "unsupported resolution kind",
            )),
        }
    }
}
