//! Checkpoint v2 state and store contract.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::budget::BudgetUsageSnapshot;
use crate::checkpoint::{
    canonical_json_bytes, validate_checkpoint_key, validate_extension_namespace, validate_sha256,
    CheckpointError, CheckpointResult, CheckpointStatus, ClaimMode, EventCursor, OperationKind,
    OperationState, ToolIdempotency, MAX_EXTENSION_ENTRY_BYTES, MAX_WIRE_INTEGER,
};
use crate::events::RunEvent;
use crate::types::{CycleRecord, Message};

mod transitions;
mod validation;

pub use transitions::*;
pub use validation::*;
use validation::{optional_string, required_string, required_u64, validate_json};

pub const CHECKPOINT_SCHEMA: &str = crate::checkpoint::CHECKPOINT_SCHEMA;
pub const RUN_DEFINITION_SCHEMA: &str = crate::checkpoint::RUN_DEFINITION_SCHEMA;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationError {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
}

impl OperationError {
    pub fn new(code: impl Into<String>, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable,
        }
    }

    pub fn validate(&self) -> CheckpointResult<()> {
        if self.code.trim().is_empty() || self.message.trim().is_empty() {
            return Err(CheckpointError::new(
                "operation_error_invalid",
                "operation error code and message must be non-empty",
            ));
        }
        Ok(())
    }

    pub fn to_value(&self) -> Value {
        serde_json::json!({
            "code": self.code,
            "message": self.message,
            "retryable": self.retryable,
        })
    }

    pub fn from_value(value: &Value) -> CheckpointResult<Self> {
        let object = value.as_object().ok_or_else(|| {
            CheckpointError::new(
                "operation_error_invalid",
                "operation error must be an object",
            )
        })?;
        let error = Self {
            code: required_string(object, "code", "operation_error_invalid")?.to_string(),
            message: required_string(object, "message", "operation_error_invalid")?.to_string(),
            retryable: object
                .get("retryable")
                .and_then(Value::as_bool)
                .ok_or_else(|| {
                    CheckpointError::new(
                        "operation_error_invalid",
                        "operation error retryable must be a boolean",
                    )
                })?,
        };
        error.validate()?;
        Ok(error)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct OperationJournalEntry {
    pub kind: OperationKind,
    pub operation_id: String,
    pub cycle_index: u64,
    pub attempt: u64,
    pub state: OperationState,
    pub request_digest: String,
    pub idempotency_key: Option<String>,
    pub response: Option<Value>,
    pub error: Option<OperationError>,
    pub tool_call_id: Option<String>,
    pub tool_name: Option<String>,
    pub arguments: Option<Map<String, Value>>,
    pub idempotency_support: Option<ToolIdempotency>,
    pub result: Option<Value>,
}

impl OperationJournalEntry {
    pub fn model(
        operation_id: impl Into<String>,
        cycle_index: u64,
        attempt: u64,
        request_digest: impl Into<String>,
        idempotency_key: Option<String>,
    ) -> Self {
        Self {
            kind: OperationKind::Model,
            operation_id: operation_id.into(),
            cycle_index,
            attempt,
            state: OperationState::Planned,
            request_digest: request_digest.into(),
            idempotency_key,
            response: None,
            error: None,
            tool_call_id: None,
            tool_name: None,
            arguments: None,
            idempotency_support: None,
            result: None,
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
        idempotency_key: impl Into<String>,
        idempotency_support: ToolIdempotency,
    ) -> Self {
        Self {
            kind: OperationKind::Tool,
            operation_id: operation_id.into(),
            cycle_index,
            attempt,
            state: OperationState::Planned,
            request_digest: request_digest.into(),
            idempotency_key: Some(idempotency_key.into()),
            response: None,
            error: None,
            tool_call_id: Some(tool_call_id.into()),
            tool_name: Some(tool_name.into()),
            arguments: Some(arguments),
            idempotency_support: Some(idempotency_support),
            result: None,
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
                {
                    return Err(CheckpointError::new(
                        "operation_kind_fields_invalid",
                        "model journal entries cannot contain tool fields",
                    ));
                }
            }
            OperationKind::Tool => {
                if self.tool_call_id.as_deref().is_none_or(str::is_empty)
                    || self.tool_name.as_deref().is_none_or(str::is_empty)
                    || self.idempotency_key.as_deref().is_none_or(str::is_empty)
                    || self.arguments.is_none()
                    || self.idempotency_support.is_none()
                {
                    return Err(CheckpointError::new(
                        "tool_idempotency_key_required",
                        "tool journal entries require call, arguments, idempotency key, and support",
                    ));
                }
                if self.response.is_some() {
                    return Err(CheckpointError::new(
                        "operation_kind_fields_invalid",
                        "tool journal entries cannot contain model responses",
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
                | (OperationState::Ambiguous, OperationState::Planned)
                | (OperationState::Ambiguous, OperationState::Succeeded)
                | (OperationState::Ambiguous, OperationState::Failed)
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

#[derive(Debug, Clone, PartialEq)]
pub struct ExtensionStateEntry {
    pub version: String,
    pub required: bool,
    pub state: Value,
}

impl ExtensionStateEntry {
    pub fn validate(&self) -> CheckpointResult<()> {
        if self.version.trim().is_empty() {
            return Err(CheckpointError::new(
                "checkpoint_extension_state_invalid",
                "extension version must be non-empty",
            ));
        }
        validate_json(&self.state, "extension state")
    }

    pub fn to_value(&self) -> Value {
        serde_json::json!({
            "version": self.version,
            "required": self.required,
            "state": self.state,
        })
    }

    pub fn from_value(value: &Value) -> CheckpointResult<Self> {
        let object = value.as_object().ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_extension_state_invalid",
                "extension state entry must be an object",
            )
        })?;
        const FIELDS: [&str; 3] = ["version", "required", "state"];
        if object.len() != FIELDS.len() || FIELDS.iter().any(|field| !object.contains_key(*field)) {
            return Err(CheckpointError::new(
                "checkpoint_extension_state_invalid",
                "extension state entry has missing or unknown fields",
            ));
        }
        let entry = Self {
            version: required_string(object, "version", "checkpoint_extension_state_invalid")?
                .to_string(),
            required: object
                .get("required")
                .and_then(Value::as_bool)
                .ok_or_else(|| {
                    CheckpointError::new(
                        "checkpoint_extension_state_invalid",
                        "extension required must be a boolean",
                    )
                })?,
            state: object.get("state").cloned().ok_or_else(|| {
                CheckpointError::new(
                    "checkpoint_extension_state_invalid",
                    "extension state is required",
                )
            })?,
        };
        entry.validate()?;
        Ok(entry)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EventOutboxEntry {
    pub event_id: String,
    pub payload_digest: String,
    pub state: String,
    pub event: Value,
    pub cursor: Option<Value>,
}

impl EventOutboxEntry {
    pub fn validate(&self) -> CheckpointResult<()> {
        if self.event_id.trim().is_empty() {
            return Err(CheckpointError::new(
                "checkpoint_event_outbox_invalid",
                "event_id must be non-empty",
            ));
        }
        validate_sha256(&self.payload_digest, "event_outbox.payload_digest")?;
        if self.state != "pending" && self.state != "delivered" {
            return Err(CheckpointError::new(
                "checkpoint_event_outbox_invalid",
                "outbox state must be pending or delivered",
            ));
        }
        if !self.event.is_object() {
            return Err(CheckpointError::new(
                "checkpoint_event_outbox_invalid",
                "outbox event must be an object",
            ));
        }
        let event: RunEvent = serde_json::from_value(self.event.clone()).map_err(|_| {
            CheckpointError::new(
                "checkpoint_event_invalid",
                "outbox event must match the current RunEvent wire contract",
            )
        })?;
        if event.event_id().as_str() != self.event_id {
            return Err(CheckpointError::new(
                "event_identity_conflict",
                "outbox event_id must match the embedded RunEvent event_id",
            ));
        }
        let canonical = serde_json::to_value(event).map_err(|_| {
            CheckpointError::new(
                "checkpoint_event_invalid",
                "outbox event could not be encoded as the current RunEvent wire contract",
            )
        })?;
        if canonical != self.event {
            return Err(CheckpointError::new(
                "checkpoint_event_invalid",
                "outbox event must use the canonical current RunEvent shape",
            ));
        }
        if self.state == "pending" && self.cursor.is_some()
            || self.state == "delivered" && self.cursor.is_none()
        {
            return Err(CheckpointError::new(
                "checkpoint_event_outbox_invalid",
                "pending entries have no cursor and delivered entries have one",
            ));
        }
        if let Some(cursor) = &self.cursor {
            validate_json(cursor, "event outbox cursor")?;
        }
        Ok(())
    }

    pub fn verify_payload(&self) -> CheckpointResult<()> {
        self.validate()?;
        let digest = crate::checkpoint::event_payload_digest(&self.event)?;
        if digest != self.payload_digest {
            return Err(CheckpointError::new(
                "event_identity_conflict",
                "outbox payload digest does not match event",
            ));
        }
        Ok(())
    }

    pub fn pending(event_id: impl Into<String>, event: Value) -> CheckpointResult<Self> {
        let payload_digest = crate::checkpoint::event_payload_digest(&event)?;
        let entry = Self {
            event_id: event_id.into(),
            payload_digest,
            state: "pending".to_string(),
            event,
            cursor: None,
        };
        entry.validate()?;
        Ok(entry)
    }

    pub fn to_value(&self) -> Value {
        serde_json::json!({
            "event_id": self.event_id,
            "payload_digest": self.payload_digest,
            "state": self.state,
            "event": self.event,
            "cursor": self.cursor,
        })
    }

    pub fn from_value(value: &Value) -> CheckpointResult<Self> {
        let object = value.as_object().ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_event_outbox_invalid",
                "outbox entry must be an object",
            )
        })?;
        const FIELDS: [&str; 5] = ["event_id", "payload_digest", "state", "event", "cursor"];
        if object.len() != FIELDS.len() || FIELDS.iter().any(|field| !object.contains_key(*field)) {
            return Err(CheckpointError::new(
                "checkpoint_event_outbox_invalid",
                "outbox entry has missing or unknown fields",
            ));
        }
        let entry = Self {
            event_id: required_string(object, "event_id", "checkpoint_event_outbox_invalid")?
                .to_string(),
            payload_digest: required_string(
                object,
                "payload_digest",
                "checkpoint_event_outbox_invalid",
            )?
            .to_string(),
            state: required_string(object, "state", "checkpoint_event_outbox_invalid")?.to_string(),
            event: object.get("event").cloned().ok_or_else(|| {
                CheckpointError::new(
                    "checkpoint_event_outbox_invalid",
                    "outbox event is required",
                )
            })?,
            cursor: object
                .get("cursor")
                .filter(|value| !value.is_null())
                .cloned(),
        };
        entry.validate()?;
        Ok(entry)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Checkpoint {
    pub schema_version: String,
    pub run_definition_schema: String,
    pub run_definition: Value,
    pub checkpoint_key: String,
    pub task_id: String,
    pub root_run_id: String,
    pub trace_id: String,
    pub run_definition_digest: String,
    pub resume_attempt: u64,
    pub cycle_index: u64,
    pub status: CheckpointStatus,
    pub messages: Vec<Message>,
    pub cycles: Vec<CycleRecord>,
    pub shared_state: BTreeMap<String, Value>,
    pub budget_usage: Option<BudgetUsageSnapshot>,
    pub event_cursor: Option<EventCursor>,
    pub event_outbox: Vec<EventOutboxEntry>,
    pub extension_state: BTreeMap<String, ExtensionStateEntry>,
    pub model_call_journal: Vec<OperationJournalEntry>,
    pub tool_journal: Vec<OperationJournalEntry>,
    pub revision: u64,
    pub claim_token: Option<String>,
    pub claimed_cycle: Option<u64>,
    pub lease_expires_at_ms: Option<u64>,
    pub terminal_result: Option<Value>,
    pub terminal_acknowledged: bool,
}

impl Default for Checkpoint {
    fn default() -> Self {
        Self {
            schema_version: CHECKPOINT_SCHEMA.to_string(),
            run_definition_schema: RUN_DEFINITION_SCHEMA.to_string(),
            run_definition: Value::Object(Map::new()),
            checkpoint_key: String::new(),
            task_id: String::new(),
            root_run_id: String::new(),
            trace_id: String::new(),
            run_definition_digest: String::new(),
            resume_attempt: 1,
            cycle_index: 0,
            status: CheckpointStatus::Running,
            messages: Vec::new(),
            cycles: Vec::new(),
            shared_state: BTreeMap::new(),
            budget_usage: None,
            event_cursor: None,
            event_outbox: Vec::new(),
            extension_state: BTreeMap::new(),
            model_call_journal: Vec::new(),
            tool_journal: Vec::new(),
            revision: 0,
            claim_token: None,
            claimed_cycle: None,
            lease_expires_at_ms: None,
            terminal_result: None,
            terminal_acknowledged: false,
        }
    }
}

impl Checkpoint {
    pub fn validate(&self) -> CheckpointResult<()> {
        validate_checkpoint(self)
    }

    pub fn active_cycle(&self) -> CheckpointResult<u64> {
        self.claimed_cycle
            .or_else(|| self.cycle_index.checked_add(1))
            .ok_or_else(|| {
                CheckpointError::new("checkpoint_cycle_invalid", "active cycle overflow")
            })
            .and_then(|cycle| {
                if cycle == 0 || cycle > MAX_WIRE_INTEGER {
                    Err(CheckpointError::new(
                        "checkpoint_cycle_invalid",
                        "active cycle is outside the JSON-safe range",
                    ))
                } else {
                    Ok(cycle)
                }
            })
    }

    pub fn has_ambiguous_operation(&self) -> bool {
        self.model_call_journal
            .iter()
            .chain(self.tool_journal.iter())
            .any(|entry| entry.state == OperationState::Ambiguous)
    }

    pub fn is_operator_abort_terminal(&self) -> bool {
        let Some(result) = self.terminal_result.as_ref().and_then(Value::as_object) else {
            return false;
        };
        result
            .get("error_code")
            .and_then(Value::as_str)
            .is_some_and(|code| code == "operator_abort_with_unknown_outcome")
            || result
                .get("resume_observation")
                .is_some_and(|value| !value.is_null())
    }
}

pub trait CheckpointStore: Send + Sync {
    fn create_checkpoint(&self, checkpoint: Checkpoint) -> CheckpointResult<bool>;
    fn load_checkpoint(&self, checkpoint_key: &str) -> CheckpointResult<Option<Checkpoint>>;
    fn claim_checkpoint(
        &self,
        checkpoint_key: &str,
        cycle_index: u64,
        claim_token: &str,
        lease_expires_at_ms: u64,
        now_ms: u64,
        claim_mode: ClaimMode,
    ) -> CheckpointResult<Option<Checkpoint>>;
    fn progress_checkpoint(
        &self,
        checkpoint: Checkpoint,
        claim_token: &str,
        expected_revision: u64,
    ) -> CheckpointResult<bool>;
    fn suspend_checkpoint(
        &self,
        checkpoint: Checkpoint,
        claim_token: &str,
        expected_revision: u64,
    ) -> CheckpointResult<bool>;
    fn commit_checkpoint(
        &self,
        checkpoint: Checkpoint,
        claim_token: &str,
        expected_revision: u64,
    ) -> CheckpointResult<bool>;
    fn finalize_claimed_checkpoint(
        &self,
        checkpoint: Checkpoint,
        claim_token: &str,
        expected_revision: u64,
    ) -> CheckpointResult<bool>;
    fn finalize_checkpoint(
        &self,
        checkpoint: Checkpoint,
        expected_revision: u64,
    ) -> CheckpointResult<bool>;
    fn renew_checkpoint_claim(
        &self,
        checkpoint_key: &str,
        claim_token: &str,
        lease_expires_at_ms: u64,
        now_ms: u64,
    ) -> CheckpointResult<bool>;
    fn record_event_delivery(
        &self,
        checkpoint_key: &str,
        claim_token: Option<&str>,
        expected_revision: u64,
        event_id: &str,
        payload_digest: &str,
        cursor: EventCursor,
    ) -> CheckpointResult<bool>;
    fn acknowledge_terminal(
        &self,
        checkpoint_key: &str,
        expected_revision: u64,
    ) -> CheckpointResult<bool>;
    fn delete_checkpoint(&self, checkpoint_key: &str) -> CheckpointResult<()>;
    fn list_checkpoints(&self) -> CheckpointResult<Vec<String>>;
}
