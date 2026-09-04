//! Durable deferred-tool values and the closed resolution wires.
//!
//! The framework deliberately keeps this module provider-neutral.  A deferred
//! handle is an opaque framework identity; provider/job identifiers and
//! callback policy belong to the host adapter and never cross this boundary.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{canonical_json_bytes, CheckpointError, CheckpointResult};
use crate::types::{ToolExecutionResult, ToolResultStatus};

pub const DEFERRED_HANDLE_SCHEMA: &str = "vv-agent.deferred-tool-handle.v2";
pub const TOOL_CALL_OUTCOME_SCHEMA: &str = "vv-agent.tool-call-outcome.v2";
pub const DEFERRED_RESOLVE_DECISION_SCHEMA: &str = "vv-agent.deferred-resolve-decision.v1";
pub const RECONCILIATION_DECISION_SCHEMA: &str = "vv-agent.reconciliation-decision.v1";

pub(crate) fn is_ambiguous_tool_result(result: &ToolExecutionResult) -> bool {
    if result.status != ToolResultStatus::Error {
        return false;
    }
    let definitive = result
        .metadata
        .get("definitive_outcome")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let ambiguous_code = result.error_code.as_deref().is_some_and(|code| {
        matches!(
            code,
            "tool_timeout"
                | "tool_cancelled"
                | "tool_connection_lost"
                | "tool_execution_failed"
                | "tool_orchestrator_error"
        )
    });
    ambiguous_code && !definitive
}

/// The only framework identity carried by a deferred provider callback.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct DeferredToolHandle {
    pub schema_version: String,
    pub checkpoint_key: String,
    pub operation_id: String,
    pub attempt: u64,
    pub request_digest: String,
}

impl<'de> Deserialize<'de> for DeferredToolHandle {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let object = value
            .as_object()
            .ok_or_else(|| serde::de::Error::custom("deferred_handle_invalid"))?;
        const FIELDS: [&str; 5] = [
            "schema_version",
            "checkpoint_key",
            "operation_id",
            "attempt",
            "request_digest",
        ];
        if object.get("schema_version").and_then(Value::as_str) != Some(DEFERRED_HANDLE_SCHEMA) {
            return Err(serde::de::Error::custom(
                "deferred_handle_schema_unsupported",
            ));
        }
        if object.keys().any(|key| !FIELDS.contains(&key.as_str())) {
            return Err(serde::de::Error::custom("deferred_handle_unknown_field"));
        }
        if FIELDS.iter().any(|field| !object.contains_key(*field)) {
            return Err(serde::de::Error::custom("deferred_handle_invalid"));
        }
        let handle = Self {
            schema_version: DEFERRED_HANDLE_SCHEMA.to_string(),
            checkpoint_key: object
                .get("checkpoint_key")
                .and_then(Value::as_str)
                .ok_or_else(|| serde::de::Error::custom("deferred_handle_invalid"))?
                .to_string(),
            operation_id: object
                .get("operation_id")
                .and_then(Value::as_str)
                .ok_or_else(|| serde::de::Error::custom("deferred_handle_invalid"))?
                .to_string(),
            attempt: object
                .get("attempt")
                .cloned()
                .ok_or_else(|| serde::de::Error::custom("deferred_handle_invalid"))
                .and_then(|value| {
                    serde_json::from_value(value).map_err(serde::de::Error::custom)
                })?,
            request_digest: object
                .get("request_digest")
                .and_then(Value::as_str)
                .ok_or_else(|| serde::de::Error::custom("deferred_handle_invalid"))?
                .to_string(),
        };
        handle.validate().map_err(serde::de::Error::custom)?;
        Ok(handle)
    }
}

impl DeferredToolHandle {
    pub fn new(
        checkpoint_key: impl Into<String>,
        operation_id: impl Into<String>,
        attempt: u64,
        request_digest: impl Into<String>,
    ) -> CheckpointResult<Self> {
        let handle = Self {
            schema_version: DEFERRED_HANDLE_SCHEMA.to_string(),
            checkpoint_key: checkpoint_key.into(),
            operation_id: operation_id.into(),
            attempt,
            request_digest: request_digest.into(),
        };
        handle.validate()?;
        Ok(handle)
    }

    pub fn validate(&self) -> CheckpointResult<()> {
        if self.schema_version != DEFERRED_HANDLE_SCHEMA {
            return Err(CheckpointError::new(
                "deferred_handle_schema_unsupported",
                "deferred handle schema_version is unsupported",
            ));
        }
        if self.checkpoint_key.trim().is_empty() {
            return Err(CheckpointError::new(
                "deferred_handle_invalid",
                "deferred handle checkpoint_key must be non-empty",
            ));
        }
        if self.operation_id.trim().is_empty() {
            return Err(CheckpointError::new(
                "deferred_handle_invalid",
                "deferred handle operation_id must be non-empty",
            ));
        }
        if self.attempt == 0 || self.attempt > super::MAX_WIRE_INTEGER {
            return Err(CheckpointError::new(
                "deferred_handle_invalid",
                "deferred handle attempt must be a positive JSON-safe integer",
            ));
        }
        super::validate_sha256(&self.request_digest, "deferred handle request_digest")
    }

    pub fn handle_key(&self) -> CheckpointResult<String> {
        self.validate()?;
        canonical_sha256(
            &serde_json::to_value(self).map_err(|error| {
                CheckpointError::new("deferred_handle_invalid", error.to_string())
            })?,
            "deferred handle",
        )
    }
}

/// The closed result-or-deferred outcome returned by a tool invocation.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum ToolCallOutcome {
    Completed { result: ToolExecutionResult },
    Deferred { handle: DeferredToolHandle },
}

impl Serialize for ToolCallOutcome {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let value = match self {
            Self::Completed { result } => serde_json::json!({
                "schema_version": TOOL_CALL_OUTCOME_SCHEMA,
                "kind": "completed",
                "result": result,
            }),
            Self::Deferred { handle } => serde_json::json!({
                "schema_version": TOOL_CALL_OUTCOME_SCHEMA,
                "kind": "deferred",
                "handle": handle,
            }),
        };
        value.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ToolCallOutcome {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let object = value
            .as_object()
            .ok_or_else(|| serde::de::Error::custom("tool_call_outcome_invalid"))?;
        let schema = object
            .get("schema_version")
            .and_then(Value::as_str)
            .ok_or_else(|| serde::de::Error::custom("tool_call_outcome_invalid"))?;
        if schema != TOOL_CALL_OUTCOME_SCHEMA {
            return Err(serde::de::Error::custom(
                "tool_call_outcome_schema_unsupported",
            ));
        }
        let kind = object
            .get("kind")
            .and_then(Value::as_str)
            .ok_or_else(|| serde::de::Error::custom("tool_call_outcome_invalid"))?;
        let expected = match kind {
            "completed" => ["schema_version", "kind", "result"].as_slice(),
            "deferred" => ["schema_version", "kind", "handle"].as_slice(),
            _ => return Err(serde::de::Error::custom("tool_call_outcome_invalid")),
        };
        if object.keys().any(|key| !expected.contains(&key.as_str()))
            || expected.iter().any(|key| !object.contains_key(*key))
        {
            return Err(serde::de::Error::custom(
                "tool_call_outcome_unknown_or_missing_field",
            ));
        }
        let outcome = match kind {
            "completed" => Self::Completed {
                result: serde_json::from_value(
                    object.get("result").cloned().expect("checked above"),
                )
                .map_err(serde::de::Error::custom)?,
            },
            "deferred" => Self::Deferred {
                handle: serde_json::from_value(
                    object.get("handle").cloned().expect("checked above"),
                )
                .map_err(serde::de::Error::custom)?,
            },
            _ => unreachable!("kind validated above"),
        };
        outcome.validate().map_err(serde::de::Error::custom)?;
        Ok(outcome)
    }
}

impl ToolCallOutcome {
    pub fn completed(result: ToolExecutionResult) -> Self {
        Self::Completed { result }
    }

    pub fn deferred(handle: DeferredToolHandle) -> Self {
        Self::Deferred { handle }
    }

    pub fn result(&self) -> Option<&ToolExecutionResult> {
        match self {
            Self::Completed { result } => Some(result),
            Self::Deferred { .. } => None,
        }
    }

    pub fn handle(&self) -> Option<&DeferredToolHandle> {
        match self {
            Self::Completed { .. } => None,
            Self::Deferred { handle } => Some(handle),
        }
    }

    pub fn validate(&self) -> CheckpointResult<()> {
        match self {
            Self::Completed { result } => {
                result
                    .validate()
                    .map_err(|error| CheckpointError::new("tool_call_outcome_invalid", error))?;
                Ok(())
            }
            Self::Deferred { handle } => handle.validate(),
        }
    }

    /// Stable non-durable failure required by the contract.  This is a
    /// completed error, never a synthesized handle.
    pub fn requires_checkpoint(tool_call_id: impl Into<String>) -> Self {
        Self::Completed {
            result: ToolExecutionResult::error(
                tool_call_id,
                "Deferred execution requires a durable checkpoint.",
            )
            .with_error_code("deferred_requires_checkpoint"),
        }
    }
}

/// A definitive receipt retained independently of the checkpoint payload.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeferredReceipt {
    pub handle_key: String,
    pub handle: DeferredToolHandle,
    pub result: ToolExecutionResult,
    pub result_digest: String,
    pub event_id: String,
    pub event_payload_digest: String,
    pub receipt_status: DeferredReceiptStatus,
}

impl<'de> Deserialize<'de> for DeferredReceipt {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let object = value
            .as_object()
            .ok_or_else(|| serde::de::Error::custom("deferred_receipt_invalid"))?;
        const FIELDS: [&str; 7] = [
            "handle_key",
            "handle",
            "result",
            "result_digest",
            "event_id",
            "event_payload_digest",
            "receipt_status",
        ];
        if object.keys().any(|key| !FIELDS.contains(&key.as_str())) {
            return Err(serde::de::Error::custom("deferred_receipt_unknown_field"));
        }
        if FIELDS.iter().any(|field| !object.contains_key(*field)) {
            return Err(serde::de::Error::custom("deferred_receipt_invalid"));
        }
        let receipt = Self {
            handle_key: serde_json::from_value(
                object.get("handle_key").cloned().expect("checked above"),
            )
            .map_err(serde::de::Error::custom)?,
            handle: serde_json::from_value(object.get("handle").cloned().expect("checked above"))
                .map_err(serde::de::Error::custom)?,
            result: serde_json::from_value(object.get("result").cloned().expect("checked above"))
                .map_err(serde::de::Error::custom)?,
            result_digest: serde_json::from_value(
                object.get("result_digest").cloned().expect("checked above"),
            )
            .map_err(serde::de::Error::custom)?,
            event_id: serde_json::from_value(
                object.get("event_id").cloned().expect("checked above"),
            )
            .map_err(serde::de::Error::custom)?,
            event_payload_digest: serde_json::from_value(
                object
                    .get("event_payload_digest")
                    .cloned()
                    .expect("checked above"),
            )
            .map_err(serde::de::Error::custom)?,
            receipt_status: serde_json::from_value(
                object
                    .get("receipt_status")
                    .cloned()
                    .expect("checked above"),
            )
            .map_err(serde::de::Error::custom)?,
        };
        receipt.validate().map_err(serde::de::Error::custom)?;
        Ok(receipt)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeferredReceiptStatus {
    Succeeded,
    Failed,
}

impl DeferredReceipt {
    pub fn new(
        handle: DeferredToolHandle,
        result: ToolExecutionResult,
        event_id: impl Into<String>,
        event_payload_digest: impl Into<String>,
    ) -> CheckpointResult<Self> {
        handle.validate()?;
        validate_definitive_result(&result)?;
        let result_digest = result_digest(&result)?;
        let receipt_status = if result_is_success(&result) {
            DeferredReceiptStatus::Succeeded
        } else {
            DeferredReceiptStatus::Failed
        };
        let receipt = Self {
            handle_key: handle.handle_key()?,
            handle,
            result,
            result_digest,
            event_id: event_id.into(),
            event_payload_digest: event_payload_digest.into(),
            receipt_status,
        };
        receipt.validate()?;
        Ok(receipt)
    }

    pub fn validate(&self) -> CheckpointResult<()> {
        self.handle.validate()?;
        if self.handle_key != self.handle.handle_key()? {
            return Err(CheckpointError::new(
                "deferred_receipt_invalid",
                "receipt handle_key does not match the exact handle",
            ));
        }
        validate_definitive_result(&self.result)?;
        if self.result_digest != result_digest(&self.result)? {
            return Err(CheckpointError::new(
                "deferred_receipt_invalid",
                "receipt result_digest does not match the canonical result",
            ));
        }
        super::validate_sha256(&self.event_payload_digest, "receipt event_payload_digest")?;
        if self.event_id.trim().is_empty() {
            return Err(CheckpointError::new(
                "deferred_receipt_invalid",
                "receipt event_id must be non-empty",
            ));
        }
        let expected = if result_is_success(&self.result) {
            DeferredReceiptStatus::Succeeded
        } else {
            DeferredReceiptStatus::Failed
        };
        if self.receipt_status != expected {
            return Err(CheckpointError::new(
                "deferred_receipt_invalid",
                "receipt_status does not match the definitive result",
            ));
        }
        Ok(())
    }
}

/// Closed public result of `CheckpointStore::resolve_deferred`.
#[derive(Debug, Clone, PartialEq)]
pub enum DeferredResolveDecision {
    AppliedReady { receipt: DeferredReceipt },
    AppliedWaiting { receipt: DeferredReceipt },
    Replayed { receipt: DeferredReceipt },
    NotAdmitted { retryable_error: String },
    ReconciliationRequired,
}

impl Serialize for DeferredResolveDecision {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut object = serde_json::Map::from_iter([
            (
                "schema_version".to_string(),
                Value::String(DEFERRED_RESOLVE_DECISION_SCHEMA.to_string()),
            ),
            ("kind".to_string(), Value::String(self.kind().to_string())),
        ]);
        match self {
            Self::AppliedReady { receipt }
            | Self::AppliedWaiting { receipt }
            | Self::Replayed { receipt } => {
                object.insert(
                    "receipt".to_string(),
                    serde_json::to_value(receipt).map_err(serde::ser::Error::custom)?,
                );
            }
            Self::NotAdmitted { retryable_error } => {
                object.insert(
                    "retryable_error".to_string(),
                    Value::String(retryable_error.clone()),
                );
            }
            Self::ReconciliationRequired => {}
        }
        Value::Object(object).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for DeferredResolveDecision {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let object = value
            .as_object()
            .ok_or_else(|| serde::de::Error::custom("deferred_resolve_decision_invalid"))?;
        if object.get("schema_version").and_then(Value::as_str)
            != Some(DEFERRED_RESOLVE_DECISION_SCHEMA)
        {
            return Err(serde::de::Error::custom(
                "deferred_resolve_decision_invalid",
            ));
        }
        let kind = object
            .get("kind")
            .and_then(Value::as_str)
            .ok_or_else(|| serde::de::Error::custom("deferred_resolve_decision_invalid"))?;
        let expected = match kind {
            "applied_ready" | "applied_waiting" | "replayed" => {
                ["schema_version", "kind", "receipt"].as_slice()
            }
            "not_admitted" => ["schema_version", "kind", "retryable_error"].as_slice(),
            "reconciliation_required" => ["schema_version", "kind"].as_slice(),
            _ => {
                return Err(serde::de::Error::custom(
                    "deferred_resolve_decision_invalid",
                ))
            }
        };
        if object.keys().any(|key| !expected.contains(&key.as_str()))
            || expected.iter().any(|key| !object.contains_key(*key))
        {
            return Err(serde::de::Error::custom(
                "deferred_resolve_decision_invalid",
            ));
        }
        let decision = match kind {
            "applied_ready" => Self::AppliedReady {
                receipt: serde_json::from_value(
                    object.get("receipt").cloned().expect("checked above"),
                )
                .map_err(serde::de::Error::custom)?,
            },
            "applied_waiting" => Self::AppliedWaiting {
                receipt: serde_json::from_value(
                    object.get("receipt").cloned().expect("checked above"),
                )
                .map_err(serde::de::Error::custom)?,
            },
            "replayed" => Self::Replayed {
                receipt: serde_json::from_value(
                    object.get("receipt").cloned().expect("checked above"),
                )
                .map_err(serde::de::Error::custom)?,
            },
            "not_admitted" => Self::NotAdmitted {
                retryable_error: object
                    .get("retryable_error")
                    .and_then(Value::as_str)
                    .ok_or_else(|| serde::de::Error::custom("deferred_resolve_decision_invalid"))?
                    .to_string(),
            },
            "reconciliation_required" => Self::ReconciliationRequired,
            _ => unreachable!("kind validated above"),
        };
        decision.validate().map_err(serde::de::Error::custom)?;
        Ok(decision)
    }
}

impl DeferredResolveDecision {
    fn kind(&self) -> &'static str {
        match self {
            Self::AppliedReady { .. } => "applied_ready",
            Self::AppliedWaiting { .. } => "applied_waiting",
            Self::Replayed { .. } => "replayed",
            Self::NotAdmitted { .. } => "not_admitted",
            Self::ReconciliationRequired => "reconciliation_required",
        }
    }

    pub fn not_admitted() -> Self {
        Self::NotAdmitted {
            retryable_error: "deferred_resolution_not_admitted".to_string(),
        }
    }

    pub fn receipt(&self) -> Option<&DeferredReceipt> {
        match self {
            Self::AppliedReady { receipt }
            | Self::AppliedWaiting { receipt }
            | Self::Replayed { receipt } => Some(receipt),
            Self::NotAdmitted { .. } | Self::ReconciliationRequired => None,
        }
    }

    pub fn validate(&self) -> CheckpointResult<()> {
        match self {
            Self::AppliedReady { receipt }
            | Self::AppliedWaiting { receipt }
            | Self::Replayed { receipt } => receipt.validate(),
            Self::NotAdmitted { retryable_error } => {
                if retryable_error != "deferred_resolution_not_admitted" {
                    return Err(CheckpointError::new(
                        "deferred_resolve_decision_invalid",
                        "not_admitted must carry the stable retryable error code",
                    ));
                }
                Ok(())
            }
            Self::ReconciliationRequired => Ok(()),
        }
    }
}

/// A trusted, provider-neutral reconciliation decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AcceptDeferredDecision {
    pub schema_version: String,
    pub kind: String,
    pub handle: DeferredToolHandle,
}

impl<'de> Deserialize<'de> for AcceptDeferredDecision {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let object = value
            .as_object()
            .ok_or_else(|| serde::de::Error::custom("reconciliation_decision_invalid"))?;
        const FIELDS: [&str; 3] = ["schema_version", "kind", "handle"];
        if object.keys().any(|key| !FIELDS.contains(&key.as_str()))
            || FIELDS.iter().any(|field| !object.contains_key(*field))
        {
            return Err(serde::de::Error::custom("reconciliation_decision_invalid"));
        }
        let decision = Self {
            schema_version: serde_json::from_value(
                object
                    .get("schema_version")
                    .cloned()
                    .expect("checked above"),
            )
            .map_err(serde::de::Error::custom)?,
            kind: serde_json::from_value(object.get("kind").cloned().expect("checked above"))
                .map_err(serde::de::Error::custom)?,
            handle: serde_json::from_value(object.get("handle").cloned().expect("checked above"))
                .map_err(serde::de::Error::custom)?,
        };
        decision.validate().map_err(serde::de::Error::custom)?;
        Ok(decision)
    }
}

impl AcceptDeferredDecision {
    pub fn new(handle: DeferredToolHandle) -> Self {
        Self {
            schema_version: RECONCILIATION_DECISION_SCHEMA.to_string(),
            kind: "accept_deferred".to_string(),
            handle,
        }
    }

    pub fn validate(&self) -> CheckpointResult<()> {
        if self.schema_version != RECONCILIATION_DECISION_SCHEMA || self.kind != "accept_deferred" {
            return Err(CheckpointError::new(
                "reconciliation_decision_invalid",
                "unsupported reconciliation decision",
            ));
        }
        self.handle.validate()
    }
}

/// One model-tool result collected before the single admission CAS.
#[derive(Debug, Clone, PartialEq)]
pub struct DeferredBatchEntry {
    pub operation_id: String,
    pub cycle_index: u64,
    pub attempt: u64,
    pub request_digest: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub idempotency_key: Option<String>,
    pub idempotency_support: crate::checkpoint::ToolIdempotency,
    pub outcome: ToolCallOutcome,
}

impl DeferredBatchEntry {
    pub fn validate(&self) -> CheckpointResult<()> {
        if self.operation_id.trim().is_empty()
            || self.tool_call_id.trim().is_empty()
            || self.tool_name.trim().is_empty()
        {
            return Err(CheckpointError::new(
                "deferred_batch_entry_invalid",
                "deferred batch identity fields must be non-empty",
            ));
        }
        if self.cycle_index == 0 || self.attempt == 0 {
            return Err(CheckpointError::new(
                "deferred_batch_entry_invalid",
                "deferred batch cycle and attempt must be positive",
            ));
        }
        super::validate_sha256(&self.request_digest, "deferred batch request_digest")?;
        self.outcome.validate()
    }
}

/// Result of an atomic admission CAS.
#[derive(Debug, Clone, PartialEq)]
pub struct DeferredBatchAdmission {
    pub checkpoint: crate::runtime::state::Checkpoint,
    pub handles: Vec<DeferredToolHandle>,
}

pub fn validate_definitive_result(result: &ToolExecutionResult) -> CheckpointResult<()> {
    if result.status == ToolResultStatus::Success && result.error_code.is_some() {
        return Err(CheckpointError::new(
            "tool_result_invalid",
            "SUCCESS results must not contain an error_code",
        ));
    }
    result
        .validate()
        .map_err(|error| CheckpointError::new("deferred_resolution_result_invalid", error))?;
    if !matches!(
        result.status,
        ToolResultStatus::Success | ToolResultStatus::Error
    ) {
        return Err(CheckpointError::new(
            "deferred_resolution_result_invalid",
            "deferred resolution requires SUCCESS or ERROR",
        ));
    }
    if is_ambiguous_tool_result(result) {
        return Err(CheckpointError::new(
            "deferred_resolution_result_invalid",
            "deferred resolution requires a definitive outcome",
        ));
    }
    Ok(())
}

pub fn result_is_success(result: &ToolExecutionResult) -> bool {
    result.status == ToolResultStatus::Success
}

pub fn result_digest(result: &ToolExecutionResult) -> CheckpointResult<String> {
    let value = serde_json::to_value(result)
        .map_err(|error| CheckpointError::new("deferred_receipt_invalid", error.to_string()))?;
    canonical_sha256(&value, "deferred result")
}

fn canonical_sha256(value: &Value, field: &str) -> CheckpointResult<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(canonical_json_bytes(value, field)?)
    ))
}

impl fmt::Display for DeferredToolHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}:{}:{}",
            self.checkpoint_key, self.operation_id, self.attempt
        )
    }
}
