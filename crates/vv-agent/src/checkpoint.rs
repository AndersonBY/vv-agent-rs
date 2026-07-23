//! Public building blocks for the current durable checkpoint protocol.
//!
//! This module owns the language-neutral values and canonical JSON helpers used
//! by definitions, journals, and event identities. The wire discriminator is
//! validated strictly; no historical checkpoint decoder is retained.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};
use sha2::{Digest, Sha256};

use crate::runtime::backends::CapabilityRef;
use crate::runtime::state::CheckpointStore;

mod canonical;

pub use canonical::*;
use canonical::{
    require_non_empty, require_positive, utf16_cmp, validate_capability_ref,
    validate_capability_slot, validate_i_json, validate_pointer,
};

pub const MAX_WIRE_INTEGER: u64 = (1_u64 << 53) - 1;
pub const MAX_CHECKPOINT_KEY_BYTES: usize = 512;
pub const MAX_EXTENSION_NAMESPACE_BYTES: usize = 128;
pub const MAX_EXTENSION_ENTRY_BYTES: usize = 65_536;
pub const DEFAULT_MAX_EXTENSION_STATE_BYTES: u64 = 262_144;
pub const CHECKPOINT_SCHEMA: &str = "vv-agent.checkpoint.v3";
pub const RUN_DEFINITION_SCHEMA: &str = "vv-agent.run-definition.v2";
pub const OPERATION_REQUEST_SCHEMA: &str = "vv-agent.operation-request.v1";
pub const EVENT_CURSOR_SCHEMA: &str = "vv-agent.event-cursor.v1";
pub const CREDENTIAL_REDACTED: &str = "<credential-redacted>";

/// A stable, observable checkpoint error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointError {
    code: String,
    message: String,
}

impl CheckpointError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn error_code(&self) -> &str {
        self.code()
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for CheckpointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for CheckpointError {}

impl From<serde_json::Error> for CheckpointError {
    fn from(error: serde_json::Error) -> Self {
        Self::new("checkpoint_json_invalid", error.to_string())
    }
}

impl From<std::io::Error> for CheckpointError {
    fn from(error: std::io::Error) -> Self {
        Self::new("checkpoint_store_io", error.to_string())
    }
}

pub type CheckpointResult<T> = Result<T, CheckpointError>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumePolicy {
    #[default]
    New,
    ResumeIfPresent,
    RequireExisting,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AmbiguousModelPolicy {
    #[default]
    RequireReconciliation,
    RetryWithDuplicateRisk,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AmbiguousToolPolicy {
    #[default]
    RequireReconciliation,
    RetryIdempotentOnly,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolIdempotency {
    Supported,
    Unsupported,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Model,
    Tool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    Planned,
    Started,
    Succeeded,
    Failed,
    Ambiguous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimMode {
    Continue,
    Recovery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointStatus {
    Pending,
    Running,
    WaitUser,
    Completed,
    Failed,
    MaxCycles,
    ReconciliationRequired,
}

impl CheckpointStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::WaitUser => "wait_user",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::MaxCycles => "max_cycles",
            Self::ReconciliationRequired => "reconciliation_required",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::WaitUser | Self::Completed | Self::Failed | Self::MaxCycles
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationDecisionKind {
    Defer,
    Retry,
    ReplaySuccess,
    RecordFailure,
    Abort,
}

/// A typed observation retained when a started external operation has no
/// durable receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeObservation {
    pub operation_id: String,
    pub operation_kind: OperationKind,
    pub cycle_index: u64,
    #[serde(default = "ambiguous_state")]
    pub state: OperationState,
    pub risk: String,
    pub idempotency_support: Option<ToolIdempotency>,
}

fn ambiguous_state() -> OperationState {
    OperationState::Ambiguous
}

impl ResumeObservation {
    pub fn validate(&self) -> CheckpointResult<()> {
        require_non_empty(&self.operation_id, "resume observation operation_id")?;
        require_positive(self.cycle_index, "resume observation cycle_index")?;
        if self.state != OperationState::Ambiguous {
            return Err(CheckpointError::new(
                "checkpoint_resume_observation_invalid",
                "resume observation state must be ambiguous",
            ));
        }
        require_non_empty(&self.risk, "resume observation risk")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconciliationError {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
}

impl ReconciliationError {
    pub fn new(code: impl Into<String>, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable,
        }
    }

    pub fn validate(&self) -> CheckpointResult<()> {
        require_non_empty(&self.code, "reconciliation error code")?;
        require_non_empty(&self.message, "reconciliation error message")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReconciliationDecision {
    pub kind: ReconciliationDecisionKind,
    pub response: Option<Value>,
    pub result: Option<Value>,
    pub error: Option<ReconciliationError>,
}

impl ReconciliationDecision {
    pub fn defer() -> Self {
        Self {
            kind: ReconciliationDecisionKind::Defer,
            response: None,
            result: None,
            error: None,
        }
    }

    pub fn retry() -> Self {
        Self {
            kind: ReconciliationDecisionKind::Retry,
            response: None,
            result: None,
            error: None,
        }
    }

    pub fn replay_response(response: Value) -> Self {
        Self {
            kind: ReconciliationDecisionKind::ReplaySuccess,
            response: Some(response),
            result: None,
            error: None,
        }
    }

    pub fn replay_result(result: Value) -> Self {
        Self {
            kind: ReconciliationDecisionKind::ReplaySuccess,
            response: None,
            result: Some(result),
            error: None,
        }
    }

    pub fn record_failure(error: ReconciliationError) -> Self {
        Self {
            kind: ReconciliationDecisionKind::RecordFailure,
            response: None,
            result: None,
            error: Some(error),
        }
    }

    pub fn abort(error: ReconciliationError) -> Self {
        Self {
            kind: ReconciliationDecisionKind::Abort,
            response: None,
            result: None,
            error: Some(error),
        }
    }

    pub fn validate(&self) -> CheckpointResult<()> {
        match self.kind {
            ReconciliationDecisionKind::ReplaySuccess
                if self.response.is_some() ^ self.result.is_some() => {}
            ReconciliationDecisionKind::ReplaySuccess => {
                return Err(CheckpointError::new(
                    "checkpoint_reconciliation_decision_invalid",
                    "replay_success requires exactly one response or result",
                ));
            }
            ReconciliationDecisionKind::RecordFailure | ReconciliationDecisionKind::Abort => {
                let Some(error) = &self.error else {
                    return Err(CheckpointError::new(
                        "checkpoint_reconciliation_decision_invalid",
                        "failure and abort decisions require a typed error",
                    ));
                };
                error.validate()?;
            }
            _ if self.response.is_none() && self.result.is_none() && self.error.is_none() => {}
            _ => {
                return Err(CheckpointError::new(
                    "checkpoint_reconciliation_decision_invalid",
                    "this reconciliation decision carries an unexpected payload",
                ));
            }
        }
        if let Some(response) = &self.response {
            validate_i_json(response, "reconciliation response")?;
        }
        if let Some(result) = &self.result {
            validate_i_json(result, "reconciliation result")?;
        }
        Ok(())
    }
}

pub trait ReconciliationProvider: Send + Sync {
    fn reconcile(
        &self,
        observation: &ResumeObservation,
    ) -> CheckpointResult<ReconciliationDecision>;
}

pub trait CheckpointExtension: Send + Sync {
    fn namespace(&self) -> &str;
    fn version(&self) -> &str;
    fn required(&self) -> bool;
    fn snapshot(&self) -> CheckpointResult<Value>;
    fn restore(&self, state: &Value) -> CheckpointResult<()>;
}

/// Runtime configuration for enabling checkpoint v2.
#[derive(Clone)]
pub struct CheckpointConfig {
    pub store: Option<Arc<dyn CheckpointStore>>,
    pub store_ref: Option<CapabilityRef>,
    pub key: Option<String>,
    pub resume_policy: ResumePolicy,
    pub ambiguous_model_policy: AmbiguousModelPolicy,
    pub ambiguous_tool_policy: AmbiguousToolPolicy,
    pub required_extension_namespaces: Vec<String>,
    pub max_extension_state_bytes: u64,
    pub credential_slots: Vec<String>,
    pub capability_refs: BTreeMap<String, CapabilityRef>,
}

impl Default for CheckpointConfig {
    fn default() -> Self {
        Self {
            store: None,
            store_ref: None,
            key: None,
            resume_policy: ResumePolicy::New,
            ambiguous_model_policy: AmbiguousModelPolicy::RequireReconciliation,
            ambiguous_tool_policy: AmbiguousToolPolicy::RequireReconciliation,
            required_extension_namespaces: Vec::new(),
            max_extension_state_bytes: DEFAULT_MAX_EXTENSION_STATE_BYTES,
            credential_slots: Vec::new(),
            capability_refs: BTreeMap::new(),
        }
    }
}

impl fmt::Debug for CheckpointConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CheckpointConfig")
            .field("store", &self.store.as_ref().map(|_| "configured"))
            .field("store_ref", &self.store_ref)
            .field("key", &self.key)
            .field("resume_policy", &self.resume_policy)
            .field("ambiguous_model_policy", &self.ambiguous_model_policy)
            .field("ambiguous_tool_policy", &self.ambiguous_tool_policy)
            .field(
                "required_extension_namespaces",
                &self.required_extension_namespaces,
            )
            .field("max_extension_state_bytes", &self.max_extension_state_bytes)
            .field("credential_slots", &self.credential_slots)
            .field("capability_refs", &self.capability_refs)
            .finish()
    }
}

impl CheckpointConfig {
    pub fn new(store: Arc<dyn CheckpointStore>) -> Self {
        Self {
            store: Some(store),
            ..Self::default()
        }
    }

    pub fn with_store<S>(store: S) -> Self
    where
        S: CheckpointStore + 'static,
    {
        Self::new(Arc::new(store))
    }

    pub fn validate(&self) -> CheckpointResult<()> {
        if self.store.is_some() == self.store_ref.is_some() {
            return Err(CheckpointError::new(
                "checkpoint_store_selection_invalid",
                "exactly one of store or store_ref is required",
            ));
        }
        if let Some(reference) = &self.store_ref {
            validate_capability_ref(reference, "CheckpointConfig.store_ref")?;
        }
        match (&self.key, self.resume_policy) {
            (None, ResumePolicy::New) => {}
            (None, _) => {
                return Err(CheckpointError::new(
                    "checkpoint_key_required",
                    "resume_if_present and require_existing need an explicit key",
                ));
            }
            (Some(key), _) => validate_checkpoint_key(key)?,
        }
        if self.max_extension_state_bytes > MAX_WIRE_INTEGER {
            return Err(CheckpointError::new(
                "checkpoint_extension_limit_invalid",
                "max_extension_state_bytes exceeds the JSON-safe integer range",
            ));
        }
        let mut previous_slot: Option<&str> = None;
        for slot in &self.credential_slots {
            validate_pointer(slot).map_err(|error| {
                CheckpointError::new("checkpoint_credential_slots_invalid", error.message)
            })?;
            if previous_slot.is_some_and(|previous| utf16_cmp(previous, slot) != Ordering::Less) {
                return Err(CheckpointError::new(
                    "checkpoint_credential_slots_invalid",
                    "credential slots must be sorted and unique",
                ));
            }
            previous_slot = Some(slot);
        }
        let mut seen = BTreeSet::new();
        for namespace in &self.required_extension_namespaces {
            validate_extension_namespace(namespace)?;
            if !seen.insert(namespace) {
                return Err(CheckpointError::new(
                    "checkpoint_extension_namespace_duplicate",
                    format!("duplicate extension namespace {namespace}"),
                ));
            }
        }
        if self
            .required_extension_namespaces
            .windows(2)
            .any(|window| window[0] > window[1])
        {
            return Err(CheckpointError::new(
                "checkpoint_extension_namespace_invalid",
                "required extension namespaces must be sorted",
            ));
        }
        for (slot, reference) in &self.capability_refs {
            validate_capability_slot(slot)?;
            validate_capability_ref(reference, &format!("capability_refs.{slot}"))?;
        }
        Ok(())
    }

    pub fn capability_ref(&self, slot: &str) -> Option<&CapabilityRef> {
        self.capability_refs.get(slot)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventCursor {
    pub schema_version: String,
    pub store_ref: CapabilityRef,
    pub value: Value,
    pub last_event_id: Option<String>,
}

impl EventCursor {
    pub fn new(store_ref: CapabilityRef, value: Value, last_event_id: Option<String>) -> Self {
        Self {
            schema_version: EVENT_CURSOR_SCHEMA.to_string(),
            store_ref,
            value,
            last_event_id,
        }
    }

    pub fn validate(&self) -> CheckpointResult<()> {
        if self.schema_version != EVENT_CURSOR_SCHEMA {
            return Err(CheckpointError::new(
                "checkpoint_event_cursor_schema_unsupported",
                "unsupported event cursor schema_version",
            ));
        }
        validate_capability_ref(&self.store_ref, "event cursor store_ref")?;
        validate_i_json(&self.value, "event cursor value")?;
        if self
            .last_event_id
            .as_ref()
            .is_some_and(|event_id| event_id.trim().is_empty())
        {
            return Err(CheckpointError::new(
                "checkpoint_event_cursor_invalid",
                "last_event_id must be non-empty when present",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppendOnceResult {
    pub inserted: bool,
    pub cursor: Value,
}

pub trait IdempotentRunEventStore: Send + Sync {
    fn append_once(
        &self,
        event_id: &str,
        payload_digest: &str,
        event: &Value,
    ) -> CheckpointResult<AppendOnceResult>;
}

/// A process-local accepted-once event store useful for deterministic tests
/// and hosts that already own their process lifetime.
#[derive(Debug, Default, Clone)]
pub struct InMemoryRunEventStore {
    entries: Arc<Mutex<RunEventEntries>>,
    next_sequence: Arc<Mutex<u64>>,
}

type RunEventEntries = BTreeMap<String, (String, Value, Value)>;

impl IdempotentRunEventStore for InMemoryRunEventStore {
    fn append_once(
        &self,
        event_id: &str,
        payload_digest: &str,
        event: &Value,
    ) -> CheckpointResult<AppendOnceResult> {
        require_non_empty(event_id, "event_id")?;
        validate_sha256(payload_digest, "payload_digest")?;
        if !event.is_object() {
            return Err(CheckpointError::new(
                "event_payload_invalid",
                "event payload must be an object",
            ));
        }
        let calculated = event_payload_digest(event)?;
        if calculated != payload_digest {
            return Err(CheckpointError::new(
                "event_payload_digest_mismatch",
                "payload_digest does not match the canonical event",
            ));
        }
        let mut entries = self.entries.lock().map_err(|_| {
            CheckpointError::new(
                "checkpoint_store_lock_poisoned",
                "event store lock poisoned",
            )
        })?;
        if let Some((existing_digest, _existing_event, cursor)) = entries.get(event_id) {
            if existing_digest != payload_digest {
                return Err(CheckpointError::new(
                    "event_identity_conflict",
                    format!("event id {event_id} was recorded with a different digest"),
                ));
            }
            return Ok(AppendOnceResult {
                inserted: false,
                cursor: cursor.clone(),
            });
        }
        let mut next = self.next_sequence.lock().map_err(|_| {
            CheckpointError::new(
                "checkpoint_store_lock_poisoned",
                "event sequence lock poisoned",
            )
        })?;
        *next = next.checked_add(1).ok_or_else(|| {
            CheckpointError::new("event_cursor_overflow", "event sequence overflow")
        })?;
        let cursor = serde_json::json!({"sequence": *next});
        entries.insert(
            event_id.to_string(),
            (payload_digest.to_string(), event.clone(), cursor.clone()),
        );
        Ok(AppendOnceResult {
            inserted: true,
            cursor,
        })
    }
}

impl crate::event_store::RunEventStore for InMemoryRunEventStore {
    fn append(
        &self,
        event: &crate::events::RunEvent,
    ) -> Result<(), crate::event_store::EventStoreError> {
        let value = serde_json::to_value(event).map_err(|error| {
            crate::event_store::EventStoreError::new(
                "event_store_serialization_error",
                error.to_string(),
            )
        })?;
        let digest = event_payload_digest(&value).map_err(|error| {
            crate::event_store::EventStoreError::new(
                "event_store_checkpoint_error",
                error.to_string(),
            )
        })?;
        IdempotentRunEventStore::append_once(self, event.event_id().as_str(), &digest, &value)
            .map_err(|error| {
                crate::event_store::EventStoreError::new(
                    "event_store_checkpoint_error",
                    error.to_string(),
                )
            })?;
        Ok(())
    }

    fn replay(
        &self,
        query: crate::event_store::RunEventReplayQuery,
    ) -> Result<crate::event_store::RunEventIter, crate::event_store::EventStoreError> {
        let entries = self.entries.lock().map_err(|_| {
            crate::event_store::EventStoreError::new(
                "event_store_lock_poisoned",
                "event store lock poisoned",
            )
        })?;
        let mut events = entries
            .values()
            .filter_map(|(_, event, cursor)| {
                let sequence = cursor.get("sequence").and_then(Value::as_u64)?;
                let event =
                    serde_json::from_value::<crate::events::RunEvent>(event.clone()).ok()?;
                let include = event.run_id() == query.run_id()
                    || (query.should_include_children()
                        && event.parent_run_id() == Some(query.run_id()));
                include.then_some((sequence, event))
            })
            .collect::<Vec<_>>();
        events.sort_by_key(|(sequence, _)| *sequence);
        Ok(Box::new(events.into_iter().map(|(_, event)| Ok(event))))
    }

    fn append_once(
        &self,
        event_id: &str,
        payload_digest: &str,
        event: &crate::events::RunEvent,
    ) -> Result<Option<EventCursor>, crate::event_store::EventStoreError> {
        let value = serde_json::to_value(event).map_err(|error| {
            crate::event_store::EventStoreError::new(
                "event_store_serialization_error",
                error.to_string(),
            )
        })?;
        let result = IdempotentRunEventStore::append_once(self, event_id, payload_digest, &value)
            .map_err(|error| {
            crate::event_store::EventStoreError::new(
                "event_store_checkpoint_error",
                error.to_string(),
            )
        })?;
        Ok(Some(EventCursor::new(
            crate::runtime::backends::CapabilityRef {
                id: "events.in-memory".to_string(),
                version: "1".to_string(),
            },
            result.cursor,
            Some(event_id.to_string()),
        )))
    }
}
