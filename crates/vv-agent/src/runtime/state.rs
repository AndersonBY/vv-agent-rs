//! Checkpoint v8 state and store contract.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::budget::BudgetUsageSnapshot;
use crate::checkpoint::{
    canonical_json_bytes, validate_checkpoint_key, validate_extension_namespace, validate_sha256,
    CheckpointError, CheckpointResult, CheckpointStatus, ClaimMode, EventCursor, OperationKind,
    OperationState, ToolIdempotency, MAX_EXTENSION_ENTRY_BYTES, MAX_WIRE_INTEGER,
};
use crate::checkpoint::{
    DeferredToolHandle, HostInteractionAdmissionContext, HostInteractionRequest, SuspendedOrigin,
};
use crate::events::RunEvent;
use crate::types::{CycleRecord, Message, ModelCallOperation, ModelCallRecord};

mod deferred;
mod journal;
mod transitions;
mod validation;

pub use deferred::{
    accept_deferred_batch, admit_deferred_batch, deferred_batch_is_idempotent, handle_key,
    receipt_event,
};
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
    pub deferred_handle: Option<DeferredToolHandle>,
    pub model_operation: Option<ModelCallOperation>,
    pub backend: Option<String>,
    pub model: Option<String>,
    pub call_id: Option<String>,
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
    pub active_host_interaction: Option<HostInteractionRequest>,
    pub suspended_origin: Option<SuspendedOrigin>,
    pub messages: Vec<Message>,
    pub cycles: Vec<CycleRecord>,
    pub model_calls: Vec<ModelCallRecord>,
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
            active_host_interaction: None,
            suspended_origin: None,
            messages: Vec::new(),
            cycles: Vec::new(),
            model_calls: Vec::new(),
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
    /// Return a stable identity for the logical backing store.
    ///
    /// Distributed runner startup uses this to prove that the process-local
    /// controller and the registry-resolved worker store are the same
    /// authority before it reads or enqueues a checkpoint. Implementations
    /// backed by cloneable handles should override the default object
    /// identity so clones of the same store compare equal.
    fn store_identity(&self) -> String {
        format!("{}:{:p}", std::any::type_name::<Self>(), self)
    }

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

    /// Atomically admit the complete model-tool batch. Implementations keep
    /// the claim until this CAS, then release it once for the whole batch.
    fn admit_deferred_batch(
        &self,
        _checkpoint_key: &str,
        _expected_revision: u64,
        _claim_token: &str,
        _claimed_cycle: u64,
        _entries: &[crate::checkpoint::DeferredBatchEntry],
    ) -> CheckpointResult<crate::checkpoint::DeferredBatchAdmission> {
        Err(CheckpointError::new(
            "checkpoint_store_deferred_unsupported",
            "checkpoint store does not implement deferred admission",
        ))
    }

    /// Resolve one admitted handle. The store owns the internal revision-CAS
    /// retry and receipt-index-first replay algorithm; callers do not supply a
    /// revision or claim token.
    fn resolve_deferred(
        &self,
        _handle: crate::checkpoint::DeferredToolHandle,
        _result: crate::types::ToolExecutionResult,
    ) -> CheckpointResult<crate::checkpoint::DeferredResolveDecision> {
        Err(CheckpointError::new(
            "checkpoint_store_deferred_unsupported",
            "checkpoint store does not implement deferred resolution",
        ))
    }

    fn accept_deferred_batch(
        &self,
        _checkpoint_key: &str,
        _expected_revision: u64,
        _claim_token: &str,
        _claimed_cycle: u64,
        _decisions: &[crate::checkpoint::AcceptDeferredDecision],
    ) -> CheckpointResult<crate::checkpoint::DeferredBatchAdmission> {
        Err(CheckpointError::new(
            "checkpoint_store_deferred_unsupported",
            "checkpoint store does not implement deferred reconciliation",
        ))
    }

    /// Admit a framework-produced host interaction against the currently
    /// claimed logical cycle.  The public producer deliberately carries only
    /// the canonical request; the store binds it to one and only one active
    /// execution claim and releases that claim in the same CAS.
    fn produce_host_interaction(
        &self,
        _request: crate::checkpoint::HostInteractionRequest,
        _context: &HostInteractionAdmissionContext,
    ) -> CheckpointResult<crate::checkpoint::HostInteractionOutcome> {
        Err(CheckpointError::new(
            "host_interaction_unsupported",
            "checkpoint store does not implement host interaction admission",
        ))
    }

    /// Admit a closed controller command and retain its durable receipt.  A
    /// replay returns the original receipt; a conflicting command id is a
    /// zero-write error.
    fn admit_controller_command(
        &self,
        _command: crate::checkpoint::ControllerCommand,
    ) -> CheckpointResult<crate::checkpoint::ControllerCommandReceipt> {
        Err(CheckpointError::new(
            "controller_command_unsupported",
            "checkpoint store does not implement controller command admission",
        ))
    }

    /// Resolve a command into its public applied/replayed/rejected envelope.
    /// Stores with a durable receipt index should override this so replay can
    /// preserve the original wake decision without producing a second outbox
    /// item.
    fn resolve_controller_command(
        &self,
        command: crate::checkpoint::ControllerCommand,
    ) -> CheckpointResult<crate::checkpoint::ControllerCommandResolution> {
        let receipt = self.admit_controller_command(command)?;
        let wake = if receipt.outbox_action == "recovery_dispatch" {
            crate::checkpoint::ControllerCommandWake::recovery(
                receipt
                    .resulting_revision
                    .checked_add(1)
                    .unwrap_or(receipt.resulting_revision),
            )
        } else {
            crate::checkpoint::ControllerCommandWake::none()
        };
        Ok(crate::checkpoint::ControllerCommandResolution::Applied { receipt, wake })
    }

    /// Read the immutable command/receipt pair for an App Server replay.  A
    /// controller action may be retried after the checkpoint has advanced, so
    /// replay must not reconstruct fences from the current checkpoint.
    fn get_controller_command_receipt(
        &self,
        _command_id: &str,
    ) -> CheckpointResult<Option<crate::checkpoint::ControllerCommandReceipt>> {
        Err(CheckpointError::new(
            "controller_command_unsupported",
            "checkpoint store does not expose controller receipts",
        ))
    }

    fn get_controller_command(
        &self,
        _command_id: &str,
    ) -> CheckpointResult<Option<crate::checkpoint::ControllerCommand>> {
        Err(CheckpointError::new(
            "controller_command_unsupported",
            "checkpoint store does not expose controller commands",
        ))
    }

    /// Consume a response at the hard recovery barrier.  Claiming the
    /// checkpoint and the response record, injecting the response, writing the
    /// consumed event, and releasing the transient record claim must be one
    /// durable CAS.
    fn claim_and_consume_host_interaction_response(
        &self,
        _envelope: crate::checkpoint::HostInteractionRecoveryEnvelope,
    ) -> CheckpointResult<crate::checkpoint::HostInteractionRecoveryResult> {
        Err(CheckpointError::new(
            "host_interaction_recovery_unsupported",
            "checkpoint store does not implement host interaction recovery",
        ))
    }

    /// Return an expired resolved response claim to `resolved_pending`.
    fn reap_host_interaction_record(
        &self,
        _record_id: &str,
        _checkpoint_key: &str,
        _now_ms: u64,
    ) -> CheckpointResult<bool> {
        Err(CheckpointError::new(
            "host_interaction_recovery_unsupported",
            "checkpoint store does not implement host interaction recovery reaping",
        ))
    }

    /// Claim one public host-interaction notification with owner/lease CAS.
    fn claim_host_interaction_notification(
        &self,
        _notification_id: &str,
        _payload_digest: &str,
        _claim_token: &str,
        _lease_expires_at_ms: u64,
        _now_ms: u64,
    ) -> CheckpointResult<Option<crate::checkpoint::HostInteractionNotificationRecord>> {
        Err(CheckpointError::new(
            "host_interaction_notification_unsupported",
            "checkpoint store does not implement host interaction notification lifecycle",
        ))
    }

    /// Read the retained public notification projection without claiming or
    /// mutating its delivery lifecycle. App Server status projections must
    /// source prompt text from this row, never from the private checkpoint.
    fn get_host_interaction_notification(
        &self,
        _notification_id: &str,
    ) -> CheckpointResult<Option<crate::checkpoint::HostInteractionNotificationRecord>> {
        Err(CheckpointError::new(
            "host_interaction_notification_unsupported",
            "checkpoint store does not expose host interaction notifications",
        ))
    }

    /// Complete a notification delivery attempt as `delivered` or `ambiguous`.
    #[allow(clippy::too_many_arguments)]
    fn complete_host_interaction_notification(
        &self,
        _notification_id: &str,
        _payload_digest: &str,
        _claim_token: &str,
        _attempt: u64,
        _outcome: &str,
        _now_ms: u64,
        _error: Option<&str>,
    ) -> CheckpointResult<Option<crate::checkpoint::HostInteractionNotificationRecord>> {
        Err(CheckpointError::new(
            "host_interaction_notification_unsupported",
            "checkpoint store does not implement host interaction notification lifecycle",
        ))
    }

    /// Resolve an ambiguous notification as `delivered`, `retry`, or `abort`.
    fn reconcile_host_interaction_notification(
        &self,
        _notification_id: &str,
        _payload_digest: &str,
        _outcome: &str,
        _now_ms: u64,
        _abort_reason: Option<&str>,
    ) -> CheckpointResult<Option<crate::checkpoint::HostInteractionNotificationRecord>> {
        Err(CheckpointError::new(
            "host_interaction_notification_unsupported",
            "checkpoint store does not implement host interaction notification lifecycle",
        ))
    }

    /// Claim the independent controller recovery-wake outbox.
    fn claim_controller_command_wake(
        &self,
        _command_id: &str,
        _command_digest: &str,
        _claim_token: &str,
        _lease_expires_at_ms: u64,
        _now_ms: u64,
    ) -> CheckpointResult<Option<crate::checkpoint::ControllerCommandReceipt>> {
        Err(CheckpointError::new(
            "controller_command_outbox_unsupported",
            "checkpoint store does not implement controller wake lifecycle",
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn complete_controller_command_wake(
        &self,
        _command_id: &str,
        _command_digest: &str,
        _claim_token: &str,
        _attempt: u64,
        _outcome: &str,
        _now_ms: u64,
        _error: Option<&str>,
    ) -> CheckpointResult<Option<crate::checkpoint::ControllerCommandReceipt>> {
        Err(CheckpointError::new(
            "controller_command_outbox_unsupported",
            "checkpoint store does not implement controller wake lifecycle",
        ))
    }

    fn reconcile_controller_command_wake(
        &self,
        _command_id: &str,
        _command_digest: &str,
        _outcome: &str,
        _now_ms: u64,
    ) -> CheckpointResult<Option<crate::checkpoint::ControllerCommandReceipt>> {
        Err(CheckpointError::new(
            "controller_command_outbox_unsupported",
            "checkpoint store does not implement controller wake lifecycle",
        ))
    }

    fn reap_controller_command_wake(
        &self,
        _command_id: &str,
        _command_digest: &str,
        _now_ms: u64,
    ) -> CheckpointResult<bool> {
        Err(CheckpointError::new(
            "controller_command_outbox_unsupported",
            "checkpoint store does not implement controller wake lifecycle",
        ))
    }
}
