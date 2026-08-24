use super::*;

impl OperationJournalEntry {
    #[allow(clippy::too_many_arguments)]
    pub fn model(
        operation_id: impl Into<String>,
        cycle_index: u64,
        attempt: u64,
        request_digest: impl Into<String>,
        model_operation: ModelCallOperation,
        backend: impl Into<String>,
        model: impl Into<String>,
        call_id: impl Into<String>,
    ) -> Self {
        Self {
            kind: OperationKind::Model,
            operation_id: operation_id.into(),
            cycle_index,
            attempt,
            state: OperationState::Planned,
            request_digest: request_digest.into(),
            idempotency_key: None,
            response: None,
            error: None,
            tool_call_id: None,
            tool_name: None,
            arguments: None,
            idempotency_support: None,
            result: None,
            deferred_handle: None,
            model_operation: Some(model_operation),
            backend: Some(backend.into()),
            model: Some(model.into()),
            call_id: Some(call_id.into()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn tool(
        operation_id: impl Into<String>,
        cycle_index: u64,
        attempt: u64,
        request_digest: impl Into<String>,
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        arguments: Map<String, Value>,
        idempotency_key: Option<String>,
        idempotency_support: ToolIdempotency,
    ) -> Self {
        Self {
            kind: OperationKind::Tool,
            operation_id: operation_id.into(),
            cycle_index,
            attempt,
            state: OperationState::Planned,
            request_digest: request_digest.into(),
            idempotency_key,
            response: None,
            error: None,
            tool_call_id: Some(tool_call_id.into()),
            tool_name: Some(tool_name.into()),
            arguments: Some(arguments),
            idempotency_support: Some(idempotency_support),
            result: None,
            deferred_handle: None,
            model_operation: None,
            backend: None,
            model: None,
            call_id: None,
        }
    }

    pub fn validate(&self) -> CheckpointResult<()> {
        if self.operation_id.trim().is_empty() {
            return Err(CheckpointError::new(
                "operation_id_invalid",
                "operation_id must be non-empty",
            ));
        }
        if self.cycle_index == 0 || self.cycle_index > MAX_WIRE_INTEGER {
            return Err(CheckpointError::new(
                "operation_cycle_invalid",
                "operation cycle_index must be positive and JSON-safe",
            ));
        }
        if self.attempt == 0 || self.attempt > MAX_WIRE_INTEGER {
            return Err(CheckpointError::new(
                "operation_attempt_invalid",
                "operation attempt must be positive and JSON-safe",
            ));
        }
        validate_sha256(&self.request_digest, "operation request_digest").map_err(|_| {
            CheckpointError::new(
                "operation_request_digest_invalid",
                "operation request_digest must be lowercase SHA-256",
            )
        })?;
        if let Some(response) = &self.response {
            validate_json(response, "operation response")?;
        }
        if let Some(result) = &self.result {
            validate_json(result, "operation result")?;
        }
        if let Some(handle) = &self.deferred_handle {
            handle.validate()?;
            if self.kind != OperationKind::Tool {
                return Err(CheckpointError::new(
                    "operation_kind_fields_invalid",
                    "model journal entries cannot contain deferred_handle",
                ));
            }
        }
        if let Some(error) = &self.error {
            error.validate()?;
        }
        match self.kind {
            OperationKind::Model => {
                if self.tool_call_id.is_some()
                    || self.tool_name.is_some()
                    || self.arguments.is_some()
                    || self.idempotency_support.is_some()
                    || self.result.is_some()
                    || self.deferred_handle.is_some()
                {
                    return Err(CheckpointError::new(
                        "operation_kind_fields_invalid",
                        "model journal entries cannot contain tool fields",
                    ));
                }
                if self.model_operation.is_none()
                    || self
                        .backend
                        .as_deref()
                        .is_none_or(|value| value.trim().is_empty())
                    || self
                        .model
                        .as_deref()
                        .is_none_or(|value| value.trim().is_empty())
                    || self
                        .call_id
                        .as_deref()
                        .is_none_or(|value| value.trim().is_empty())
                {
                    return Err(CheckpointError::new(
                        "model_identity_invalid",
                        "model journal entries require operation, backend, model, and call_id",
                    ));
                }
            }
            OperationKind::Tool => {
                if self.model_operation.is_some()
                    || self.backend.is_some()
                    || self.model.is_some()
                    || self.call_id.is_some()
                {
                    return Err(CheckpointError::new(
                        "operation_kind_fields_invalid",
                        "tool journal entries cannot contain model identity fields",
                    ));
                }
                if self.tool_call_id.as_deref().is_none_or(str::is_empty)
                    || self.tool_name.as_deref().is_none_or(str::is_empty)
                    || self.arguments.is_none()
                    || self.idempotency_support.is_none()
                {
                    return Err(CheckpointError::new(
                        "tool_idempotency_key_required",
                        "tool journal entries require call, arguments, and idempotency support",
                    ));
                }
                match self.idempotency_support.expect("checked above") {
                    ToolIdempotency::Unsupported => {
                        if self.idempotency_key.is_some() {
                            return Err(CheckpointError::new(
                                "tool_idempotency_key_invalid",
                                "unsupported tools must not carry an idempotency key",
                            ));
                        }
                    }
                    ToolIdempotency::Supported | ToolIdempotency::Unknown => {
                        if self.idempotency_key.as_deref().is_none_or(str::is_empty) {
                            return Err(CheckpointError::new(
                                "tool_idempotency_key_required",
                                "supported or unknown tools require an idempotency key",
                            ));
                        }
                    }
                }
                if self.response.is_some() {
                    return Err(CheckpointError::new(
                        "operation_kind_fields_invalid",
                        "tool journal entries cannot contain model responses",
                    ));
                }
                if self.state == OperationState::Deferred && self.deferred_handle.is_none() {
                    return Err(CheckpointError::new(
                        "operation_deferred_handle_required",
                        "deferred tool journal entries require deferred_handle",
                    ));
                }
                if self.state != OperationState::Deferred && self.deferred_handle.is_some() {
                    return Err(CheckpointError::new(
                        "operation_deferred_handle_invalid",
                        "deferred_handle is only valid for deferred entries",
                    ));
                }
                validate_json(
                    &Value::Object(self.arguments.clone().expect("checked above")),
                    "tool journal arguments",
                )?;
            }
        }
        match self.state {
            OperationState::Succeeded => {
                let receipt = match self.kind {
                    OperationKind::Model => self.response.is_some(),
                    OperationKind::Tool => self.result.is_some(),
                };
                if !receipt || self.error.is_some() {
                    return Err(CheckpointError::new(
                        "operation_receipt_required",
                        "succeeded operation requires one success receipt",
                    ));
                }
            }
            OperationState::Failed => {
                if self.error.is_none() || self.response.is_some() || self.result.is_some() {
                    return Err(CheckpointError::new(
                        "operation_error_required",
                        "failed operation requires one typed error",
                    ));
                }
            }
            OperationState::Planned | OperationState::Started | OperationState::Ambiguous => {
                if self.response.is_some() || self.result.is_some() || self.error.is_some() {
                    return Err(CheckpointError::new(
                        "operation_receipt_unexpected",
                        "non-terminal operation cannot contain a receipt",
                    ));
                }
            }
            OperationState::Deferred => {
                if self.result.is_some() || self.error.is_some() || self.deferred_handle.is_none() {
                    return Err(CheckpointError::new(
                        "operation_deferred_invalid",
                        "deferred operation requires a handle and no result or error",
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn to_value(&self) -> Value {
        let mut object = Map::new();
        object.insert("kind".to_string(), serde_json::json!(self.kind));
        object.insert(
            "operation_id".to_string(),
            Value::String(self.operation_id.clone()),
        );
        object.insert("cycle_index".to_string(), Value::from(self.cycle_index));
        object.insert("attempt".to_string(), Value::from(self.attempt));
        object.insert("state".to_string(), serde_json::json!(self.state));
        object.insert(
            "request_digest".to_string(),
            Value::String(self.request_digest.clone()),
        );
        object.insert(
            "idempotency_key".to_string(),
            self.idempotency_key
                .clone()
                .map_or(Value::Null, Value::String),
        );
        match self.kind {
            OperationKind::Model => {
                object.insert(
                    "response".to_string(),
                    self.response.clone().unwrap_or(Value::Null),
                );
                object.insert(
                    "model_operation".to_string(),
                    serde_json::to_value(self.model_operation.expect("validated model operation"))
                        .expect("model operation serializes"),
                );
                object.insert(
                    "backend".to_string(),
                    self.backend.clone().map_or(Value::Null, Value::String),
                );
                object.insert(
                    "model".to_string(),
                    self.model.clone().map_or(Value::Null, Value::String),
                );
                object.insert(
                    "call_id".to_string(),
                    self.call_id.clone().map_or(Value::Null, Value::String),
                );
            }
            OperationKind::Tool => {
                object.insert(
                    "tool_call_id".to_string(),
                    self.tool_call_id.clone().map_or(Value::Null, Value::String),
                );
                object.insert(
                    "tool_name".to_string(),
                    self.tool_name.clone().map_or(Value::Null, Value::String),
                );
                object.insert(
                    "arguments".to_string(),
                    self.arguments.clone().map_or(Value::Null, Value::Object),
                );
                object.insert(
                    "idempotency_support".to_string(),
                    self.idempotency_support
                        .map_or(Value::Null, |support| serde_json::json!(support)),
                );
                object.insert(
                    "result".to_string(),
                    self.result.clone().unwrap_or(Value::Null),
                );
                if let Some(handle) = &self.deferred_handle {
                    object.insert(
                        "deferred_handle".to_string(),
                        serde_json::to_value(handle).expect("deferred handle serializes"),
                    );
                }
            }
        }
        object.insert(
            "error".to_string(),
            self.error
                .as_ref()
                .map(OperationError::to_value)
                .unwrap_or(Value::Null),
        );
        Value::Object(object)
    }

    pub fn from_value(value: &Value) -> CheckpointResult<Self> {
        let object = value.as_object().ok_or_else(|| {
            CheckpointError::new(
                "operation_journal_invalid",
                "operation journal entry must be an object",
            )
        })?;
        const FIELDS: &[&str] = &[
            "kind",
            "operation_id",
            "cycle_index",
            "attempt",
            "request_digest",
            "tool_call_id",
            "tool_name",
            "arguments",
            "idempotency_key",
            "idempotency_support",
            "state",
            "response",
            "result",
            "error",
            "deferred_handle",
            "model_operation",
            "backend",
            "model",
            "call_id",
        ];
        if let Some(field) = object
            .keys()
            .find(|field| !FIELDS.contains(&field.as_str()))
        {
            return Err(CheckpointError::new(
                "operation_journal_invalid",
                format!("operation journal contains unknown field: {field}"),
            ));
        }
        let kind: OperationKind =
            serde_json::from_value(object.get("kind").cloned().ok_or_else(|| {
                CheckpointError::new("operation_journal_invalid", "kind missing")
            })?)
            .map_err(|_| {
                CheckpointError::new("operation_journal_invalid", "unknown operation kind")
            })?;
        let state: OperationState =
            serde_json::from_value(object.get("state").cloned().ok_or_else(|| {
                CheckpointError::new("operation_journal_invalid", "state missing")
            })?)
            .map_err(|_| {
                CheckpointError::new("operation_journal_invalid", "unknown operation state")
            })?;
        let entry = Self {
            kind,
            operation_id: required_string(object, "operation_id", "operation_journal_invalid")?
                .to_string(),
            cycle_index: required_u64(object, "cycle_index", "operation_cycle_invalid")?,
            attempt: required_u64(object, "attempt", "operation_attempt_invalid")?,
            state,
            request_digest: required_string(
                object,
                "request_digest",
                "operation_request_digest_invalid",
            )?
            .to_string(),
            idempotency_key: optional_string(object, "idempotency_key")?,
            response: object
                .get("response")
                .filter(|value| !value.is_null())
                .cloned(),
            error: object
                .get("error")
                .filter(|value| !value.is_null())
                .map(OperationError::from_value)
                .transpose()?,
            tool_call_id: optional_string(object, "tool_call_id")?,
            tool_name: optional_string(object, "tool_name")?,
            arguments: object
                .get("arguments")
                .filter(|value| !value.is_null())
                .map(|value| {
                    value.as_object().cloned().ok_or_else(|| {
                        CheckpointError::new(
                            "operation_kind_fields_invalid",
                            "tool arguments must be an object",
                        )
                    })
                })
                .transpose()?,
            idempotency_support: object
                .get("idempotency_support")
                .filter(|value| !value.is_null())
                .cloned()
                .map(|value| {
                    serde_json::from_value(value).map_err(|_| {
                        CheckpointError::new(
                            "operation_kind_fields_invalid",
                            "invalid tool idempotency support",
                        )
                    })
                })
                .transpose()?,
            result: object
                .get("result")
                .filter(|value| !value.is_null())
                .cloned(),
            deferred_handle: object
                .get("deferred_handle")
                .filter(|value| !value.is_null())
                .cloned()
                .map(|value| {
                    serde_json::from_value(value).map_err(|_| {
                        CheckpointError::new(
                            "operation_deferred_handle_invalid",
                            "invalid deferred handle",
                        )
                    })
                })
                .transpose()?,
            model_operation: object
                .get("model_operation")
                .filter(|value| !value.is_null())
                .cloned()
                .map(|value| {
                    serde_json::from_value(value).map_err(|_| {
                        CheckpointError::new(
                            "model_identity_invalid",
                            "invalid model operation identity",
                        )
                    })
                })
                .transpose()?,
            backend: optional_string(object, "backend")?,
            model: optional_string(object, "model")?,
            call_id: optional_string(object, "call_id")?,
        };
        entry.validate()?;
        Ok(entry)
    }

    pub fn transition_to(&mut self, next: OperationState) -> CheckpointResult<()> {
        let allowed = matches!(
            (self.state, next),
            (OperationState::Planned, OperationState::Failed)
                | (OperationState::Planned, OperationState::Started)
                | (OperationState::Started, OperationState::Succeeded)
                | (OperationState::Started, OperationState::Failed)
                | (OperationState::Started, OperationState::Ambiguous)
                | (OperationState::Started, OperationState::Deferred)
                | (OperationState::Ambiguous, OperationState::Planned)
                | (OperationState::Ambiguous, OperationState::Succeeded)
                | (OperationState::Ambiguous, OperationState::Failed)
                | (OperationState::Ambiguous, OperationState::Deferred)
                | (OperationState::Deferred, OperationState::Succeeded)
                | (OperationState::Deferred, OperationState::Failed)
        );
        if !allowed {
            return Err(CheckpointError::new(
                "operation_transition_invalid",
                format!("cannot transition {:?} to {:?}", self.state, next),
            ));
        }
        self.state = next;
        self.validate()
    }

    pub fn retry(&mut self) -> CheckpointResult<()> {
        if self.state != OperationState::Ambiguous {
            return Err(CheckpointError::new(
                "operation_transition_invalid",
                "only ambiguous operations can be retried",
            ));
        }
        self.attempt = self
            .attempt
            .checked_add(1)
            .ok_or_else(|| CheckpointError::new("operation_attempt_invalid", "attempt overflow"))?;
        if self.kind == OperationKind::Model {
            self.call_id = Some(format!("{}:attempt:{}", self.operation_id, self.attempt));
        }
        self.state = OperationState::Planned;
        self.validate()
    }

    pub fn mark_ambiguous(&mut self) -> CheckpointResult<()> {
        self.transition_to(OperationState::Ambiguous)
    }

    pub fn verify_request(&self, request: &Value) -> CheckpointResult<()> {
        let digest = crate::checkpoint::operation_request_digest(self.kind, request)?;
        if digest != self.request_digest {
            return Err(CheckpointError::new(
                "checkpoint_journal_integrity_mismatch",
                "operation request does not match the durable request_digest",
            ));
        }
        Ok(())
    }
}
