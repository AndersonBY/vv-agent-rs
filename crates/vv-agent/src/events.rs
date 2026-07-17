use std::time::{SystemTime, UNIX_EPOCH};

use serde::{de::Error as _, ser::Error as _, Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Number, Value};

use crate::budget::{BudgetEnforcementBoundary, BudgetExhaustion, BudgetUsageSnapshot};
use crate::checkpoint::{
    OperationKind, OperationState, ReconciliationDecisionKind, ResumeObservation, ToolIdempotency,
};
use crate::types::{AgentStatus, CompletionReason, Metadata};

mod wire;

use wire::{
    add_default_supplemental_fields, supplemental_wire_fields, validate_budget_wire_fields,
    validate_checkpoint_wire_fields, validate_completion_wire_fields,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunEventVersion(String);

impl RunEventVersion {
    pub fn v1() -> Self {
        Self("v1".to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl Default for RunEventVersion {
    fn default() -> Self {
        Self::v1()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventId(String);

impl EventId {
    pub fn new() -> Self {
        Self(format!("evt_{}", uuid::Uuid::new_v4().simple()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn stable(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err("event id must be non-empty".to_string());
        }
        Ok(Self(value))
    }
}

impl Default for EventId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunEvent {
    pub version: RunEventVersion,
    pub event_id: EventId,
    pub run_id: String,
    pub trace_id: String,
    pub session_id: Option<String>,
    pub parent_event_id: Option<String>,
    pub parent_run_id: Option<String>,
    pub created_at: f64,
    created_at_wire: CreatedAtWire,
    pub cycle_index: Option<u32>,
    pub agent_name: Option<String>,
    pub metadata: Metadata,
    pub payload: RunEventPayload,
    extra_fields: Metadata,
}

#[derive(Debug, Clone, Default)]
struct CreatedAtWire(Option<Number>);

impl PartialEq for CreatedAtWire {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl Serialize for RunEvent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut object = serde_json::to_value(&self.payload)
            .map_err(S::Error::custom)?
            .as_object()
            .cloned()
            .ok_or_else(|| S::Error::custom("run event payload must serialize as an object"))?;
        object.extend(self.extra_fields.clone());
        object.insert(
            "version".to_string(),
            serde_json::to_value(&self.version).map_err(S::Error::custom)?,
        );
        object.insert(
            "event_id".to_string(),
            serde_json::to_value(&self.event_id).map_err(S::Error::custom)?,
        );
        object.insert("run_id".to_string(), Value::String(self.run_id.clone()));
        object.insert("trace_id".to_string(), Value::String(self.trace_id.clone()));
        if let Some(session_id) = &self.session_id {
            object.insert("session_id".to_string(), Value::String(session_id.clone()));
        }
        if let Some(parent_event_id) = &self.parent_event_id {
            object.insert(
                "parent_event_id".to_string(),
                Value::String(parent_event_id.clone()),
            );
        }
        if let Some(parent_run_id) = &self.parent_run_id {
            object.insert(
                "parent_run_id".to_string(),
                Value::String(parent_run_id.clone()),
            );
        }
        let created_at = self
            .created_at_wire
            .0
            .clone()
            .or_else(|| Number::from_f64(self.created_at))
            .ok_or_else(|| S::Error::custom("created_at must be finite"))?;
        object.insert("created_at".to_string(), Value::Number(created_at));
        if let Some(cycle_index) = self.cycle_index {
            object.insert("cycle_index".to_string(), Value::from(cycle_index));
        }
        if let Some(agent_name) = &self.agent_name {
            object.insert("agent_name".to_string(), Value::String(agent_name.clone()));
        }
        if !self.metadata.is_empty() {
            object.insert(
                "metadata".to_string(),
                Value::Object(self.metadata.clone().into_iter().collect()),
            );
        }
        Value::Object(object).serialize(serializer)
    }
}

#[derive(Deserialize)]
struct RunEventWire {
    version: RunEventVersion,
    event_id: EventId,
    run_id: String,
    trace_id: String,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    parent_event_id: Option<String>,
    #[serde(default)]
    parent_run_id: Option<String>,
    #[serde(default)]
    created_at: Option<f64>,
    #[serde(default)]
    created_at_ms: Option<f64>,
    #[serde(default)]
    cycle_index: Option<u32>,
    #[serde(default)]
    agent_name: Option<String>,
    #[serde(default)]
    metadata: Metadata,
    #[serde(flatten)]
    payload: RunEventPayload,
}

impl<'de> Deserialize<'de> for RunEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let mut wire: RunEventWire =
            serde_json::from_value(value.clone()).map_err(D::Error::custom)?;
        if wire.version.as_str() != "v1" {
            return Err(D::Error::custom(format!(
                "unsupported run event version `{}`",
                wire.version.as_str()
            )));
        }
        for (field, value) in [
            ("event_id", wire.event_id.as_str()),
            ("run_id", wire.run_id.as_str()),
            ("trace_id", wire.trace_id.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(D::Error::custom(format!(
                    "run event {field} must be a non-empty string"
                )));
            }
        }
        validate_completion_wire_fields(&value).map_err(D::Error::custom)?;
        validate_budget_wire_fields(&value).map_err(D::Error::custom)?;
        validate_checkpoint_wire_fields(&wire.payload, wire.cycle_index)
            .map_err(D::Error::custom)?;
        if matches!(
            &wire.payload,
            RunEventPayload::ToolCallStarted { arguments, .. } if !arguments.is_object()
        ) {
            return Err(D::Error::custom(
                "run event tool arguments must be an object",
            ));
        }
        if let RunEventPayload::ApprovalResolved { approved, .. } = &mut wire.payload {
            let action = match value.get("action") {
                Some(Value::String(action)) => ApprovalAction::parse(action).ok_or_else(|| {
                    D::Error::custom(format!("unsupported approval action `{action}`"))
                })?,
                Some(_) => return Err(D::Error::custom("approval action must be a string")),
                None if value.get("approved").is_some() => ApprovalAction::from_approved(*approved),
                None => {
                    return Err(D::Error::custom(
                        "approval_resolved requires action or approved",
                    ))
                }
            };
            if value.get("action").is_some()
                && value.get("approved").is_some()
                && *approved != action.is_approved()
            {
                return Err(D::Error::custom(format!(
                    "approval action `{}` conflicts with approved={approved}",
                    action.as_str()
                )));
            }
            *approved = action.is_approved();
        }
        if let RunEventPayload::BudgetExhausted {
            enforcement_boundary,
            budget_exhaustion,
            ..
        } = &wire.payload
        {
            if budget_exhaustion.enforcement_boundary != *enforcement_boundary {
                return Err(D::Error::custom(
                    "run event budget exhaustion boundaries must match",
                ));
            }
        }
        let created_at_wire =
            CreatedAtWire(value.get("created_at").and_then(Value::as_number).cloned());
        let created_at = match (wire.created_at, wire.created_at_ms) {
            (Some(seconds), _) => seconds,
            (None, Some(milliseconds)) => milliseconds / 1000.0,
            (None, None) => return Err(D::Error::custom("missing created_at")),
        };
        if !created_at.is_finite() || created_at < 0.0 {
            return Err(D::Error::custom(
                "created_at must be a finite non-negative number",
            ));
        }

        let mut extra_fields = supplemental_wire_fields(&value, &wire.payload);
        add_default_supplemental_fields(&wire.payload, &mut extra_fields);
        Ok(Self {
            version: wire.version,
            event_id: wire.event_id,
            run_id: wire.run_id,
            trace_id: wire.trace_id,
            session_id: wire.session_id,
            parent_event_id: wire.parent_event_id,
            parent_run_id: wire.parent_run_id,
            created_at,
            created_at_wire,
            cycle_index: wire.cycle_index,
            agent_name: wire.agent_name,
            metadata: wire.metadata,
            payload: wire.payload,
            extra_fields,
        })
    }
}

impl RunEvent {
    pub fn new(
        run_id: impl Into<String>,
        trace_id: impl Into<String>,
        agent_name: impl Into<String>,
        cycle_index: Option<u32>,
        payload: RunEventPayload,
    ) -> Self {
        let mut extra_fields = Metadata::new();
        add_default_supplemental_fields(&payload, &mut extra_fields);
        Self {
            version: RunEventVersion::v1(),
            event_id: EventId::new(),
            run_id: run_id.into(),
            trace_id: trace_id.into(),
            session_id: None,
            parent_event_id: None,
            parent_run_id: None,
            created_at: timestamp_seconds(),
            created_at_wire: CreatedAtWire::default(),
            cycle_index,
            agent_name: Some(agent_name.into()),
            metadata: Metadata::new(),
            payload,
            extra_fields,
        }
    }

    pub fn run_started(
        run_id: impl Into<String>,
        trace_id: impl Into<String>,
        agent_name: impl Into<String>,
        input: impl Into<String>,
    ) -> Self {
        Self::new(
            run_id,
            trace_id,
            agent_name,
            None,
            RunEventPayload::RunStarted {
                input: input.into(),
            },
        )
    }

    pub fn cycle_started(
        run_id: impl Into<String>,
        trace_id: impl Into<String>,
        agent_name: impl Into<String>,
        cycle_index: u32,
    ) -> Self {
        Self::new(
            run_id,
            trace_id,
            agent_name,
            Some(cycle_index),
            RunEventPayload::CycleStarted,
        )
    }

    pub fn assistant_delta(
        run_id: impl Into<String>,
        trace_id: impl Into<String>,
        agent_name: impl Into<String>,
        cycle_index: u32,
        delta: impl Into<String>,
    ) -> Self {
        Self::new(
            run_id,
            trace_id,
            agent_name,
            Some(cycle_index),
            RunEventPayload::AssistantDelta {
                delta: delta.into(),
            },
        )
    }

    pub fn tool_call_started(
        run_id: impl Into<String>,
        trace_id: impl Into<String>,
        agent_name: impl Into<String>,
        cycle_index: u32,
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        arguments: Value,
    ) -> Self {
        Self::new(
            run_id,
            trace_id,
            agent_name,
            Some(cycle_index),
            RunEventPayload::ToolCallStarted {
                tool_call_id: tool_call_id.into(),
                tool_name: tool_name.into(),
                arguments,
            },
        )
    }

    pub fn tool_call_completed(
        run_id: impl Into<String>,
        trace_id: impl Into<String>,
        agent_name: impl Into<String>,
        cycle_index: Option<u32>,
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        status: ToolStatus,
    ) -> Self {
        Self::new(
            run_id,
            trace_id,
            agent_name,
            cycle_index,
            RunEventPayload::ToolCallCompleted {
                tool_call_id: tool_call_id.into(),
                tool_name: tool_name.into(),
                status,
            },
        )
    }

    pub fn approval_requested(
        run_id: impl Into<String>,
        trace_id: impl Into<String>,
        agent_name: impl Into<String>,
        request_id: impl Into<String>,
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::new(
            run_id,
            trace_id,
            agent_name,
            None,
            RunEventPayload::ApprovalRequested {
                request_id: request_id.into(),
                tool_call_id: tool_call_id.into(),
                tool_name: tool_name.into(),
                message: message.into(),
            },
        )
    }

    pub fn memory_compact_started(
        run_id: impl Into<String>,
        trace_id: impl Into<String>,
        agent_name: impl Into<String>,
        cycle_index: u32,
        message_count: usize,
        estimated_tokens: Option<u64>,
    ) -> Self {
        Self::new(
            run_id,
            trace_id,
            agent_name,
            Some(cycle_index),
            RunEventPayload::MemoryCompactStarted {
                message_count,
                estimated_tokens,
            },
        )
    }

    pub fn memory_compact_completed(
        run_id: impl Into<String>,
        trace_id: impl Into<String>,
        agent_name: impl Into<String>,
        cycle_index: u32,
        before_count: usize,
        after_count: usize,
        summary_tokens: Option<u64>,
    ) -> Self {
        Self::new(
            run_id,
            trace_id,
            agent_name,
            Some(cycle_index),
            RunEventPayload::MemoryCompactCompleted {
                before_count,
                after_count,
                summary_tokens,
            },
        )
    }

    pub fn handoff_completed(
        run_id: impl Into<String>,
        trace_id: impl Into<String>,
        source_agent: impl Into<String>,
        target_agent: impl Into<String>,
        tool_call_id: impl Into<String>,
    ) -> Self {
        let source_agent = source_agent.into();
        Self::new(
            run_id,
            trace_id,
            source_agent.clone(),
            None,
            RunEventPayload::HandoffCompleted {
                source_agent,
                target_agent: target_agent.into(),
                tool_call_id: tool_call_id.into(),
            },
        )
    }

    pub fn run_completed(
        run_id: impl Into<String>,
        trace_id: impl Into<String>,
        agent_name: impl Into<String>,
        status: AgentStatus,
    ) -> Self {
        Self::new(
            run_id,
            trace_id,
            agent_name,
            None,
            RunEventPayload::RunCompleted { status },
        )
    }

    pub fn run_failed(
        run_id: impl Into<String>,
        trace_id: impl Into<String>,
        agent_name: impl Into<String>,
        error: AgentErrorPayload,
    ) -> Self {
        let AgentErrorPayload { message, code } = error;
        let mut event = Self::new(
            run_id,
            trace_id,
            agent_name,
            None,
            RunEventPayload::RunFailed { error: message },
        );
        if let Some(code) = code {
            event
                .metadata
                .insert("error_code".to_string(), Value::String(code));
        }
        event
    }

    pub fn version(&self) -> &RunEventVersion {
        &self.version
    }

    pub fn event_id(&self) -> &EventId {
        &self.event_id
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub fn trace_id(&self) -> &str {
        &self.trace_id
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    pub fn parent_event_id(&self) -> Option<&str> {
        self.parent_event_id.as_deref()
    }

    pub fn parent_run_id(&self) -> Option<&str> {
        self.parent_run_id.as_deref()
    }

    pub fn created_at(&self) -> f64 {
        self.created_at
    }

    pub fn created_at_ms(&self) -> u128 {
        (self.created_at * 1000.0).round() as u128
    }

    pub fn cycle_index(&self) -> Option<u32> {
        self.cycle_index
    }

    pub fn agent_name(&self) -> Option<&str> {
        self.agent_name.as_deref()
    }

    pub fn payload(&self) -> &RunEventPayload {
        &self.payload
    }

    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    pub fn approval_action(&self) -> Option<ApprovalAction> {
        let RunEventPayload::ApprovalResolved { approved, .. } = &self.payload else {
            return None;
        };
        Some(
            self.extra_fields
                .get("action")
                .and_then(Value::as_str)
                .and_then(ApprovalAction::parse)
                .unwrap_or_else(|| ApprovalAction::from_approved(*approved)),
        )
    }

    pub fn completion_reason(&self) -> Option<CompletionReason> {
        self.extra_fields
            .get("completion_reason")
            .and_then(Value::as_str)
            .and_then(CompletionReason::parse)
    }

    pub fn completion_tool_name(&self) -> Option<&str> {
        self.extra_fields
            .get("completion_tool_name")
            .and_then(Value::as_str)
    }

    pub fn partial_output(&self) -> Option<&str> {
        self.extra_fields
            .get("partial_output")
            .and_then(Value::as_str)
    }

    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    pub fn with_event_id(mut self, event_id: impl Into<String>) -> Result<Self, String> {
        self.event_id = EventId::stable(event_id)?;
        Ok(self)
    }

    pub fn with_parent_event_id(mut self, parent_event_id: impl Into<String>) -> Self {
        self.parent_event_id = Some(parent_event_id.into());
        self
    }

    pub fn with_parent_run_id(mut self, parent_run_id: impl Into<String>) -> Self {
        self.parent_run_id = Some(parent_run_id.into());
        self
    }

    pub fn with_metadata(mut self, key: impl Into<String>, value: Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }

    pub(crate) fn with_handoff_lifecycle(
        mut self,
        status: impl Into<String>,
        child_session_id: Option<&str>,
        child_run_id: Option<&str>,
    ) -> Self {
        debug_assert!(matches!(
            &self.payload,
            RunEventPayload::HandoffStarted { .. } | RunEventPayload::HandoffCompleted { .. }
        ));
        self.extra_fields
            .insert("status".to_string(), Value::String(status.into()));
        if let Some(child_session_id) = child_session_id {
            self.extra_fields.insert(
                "child_session_id".to_string(),
                Value::String(child_session_id.to_string()),
            );
        }
        if let Some(child_run_id) = child_run_id {
            self.extra_fields.insert(
                "child_run_id".to_string(),
                Value::String(child_run_id.to_string()),
            );
        }
        self
    }

    pub(crate) fn with_sub_run_details(
        mut self,
        child_session_id: Option<&str>,
        task_id: Option<&str>,
        wait_reason: Option<&str>,
        error: Option<&str>,
        token_usage: Option<Value>,
    ) -> Self {
        debug_assert!(matches!(
            &self.payload,
            RunEventPayload::SubRunCompleted { .. }
        ));
        for (key, value) in [
            ("child_session_id", child_session_id),
            ("task_id", task_id),
            ("wait_reason", wait_reason),
            ("error", error),
        ] {
            if let Some(value) = value {
                self.extra_fields
                    .insert(key.to_string(), Value::String(value.to_string()));
            }
        }
        if let Some(token_usage) = token_usage {
            self.extra_fields
                .insert("token_usage".to_string(), token_usage);
        }
        self
    }

    pub fn with_approval_action(mut self, action: ApprovalAction) -> Self {
        if let RunEventPayload::ApprovalResolved { approved, .. } = &mut self.payload {
            *approved = action.is_approved();
            self.extra_fields.insert(
                "action".to_string(),
                Value::String(action.as_str().to_string()),
            );
        }
        self
    }

    pub fn with_final_output(mut self, final_output: Option<String>) -> Self {
        self.extra_fields.insert(
            "final_output".to_string(),
            final_output.map_or(Value::Null, Value::String),
        );
        self
    }

    pub fn with_completion_details(
        mut self,
        completion_reason: Option<CompletionReason>,
        completion_tool_name: Option<&str>,
        partial_output: Option<&str>,
    ) -> Self {
        if let Some(reason) = completion_reason {
            self.extra_fields.insert(
                "completion_reason".to_string(),
                Value::String(reason.as_str().to_string()),
            );
        }
        for (key, value) in [
            ("completion_tool_name", completion_tool_name),
            ("partial_output", partial_output),
        ] {
            if let Some(value) = value {
                self.extra_fields
                    .insert(key.to_string(), Value::String(value.to_string()));
            }
        }
        self
    }

    pub fn with_budget_details(
        mut self,
        budget_usage: Option<&BudgetUsageSnapshot>,
        budget_exhaustion: Option<&BudgetExhaustion>,
    ) -> Self {
        if let Some(budget_usage) = budget_usage {
            self.extra_fields.insert(
                "budget_usage".to_string(),
                serde_json::to_value(budget_usage).expect("budget usage serializes"),
            );
            self.extra_fields
                .entry("completion_tool_name".to_string())
                .or_insert(Value::Null);
            self.extra_fields
                .entry("partial_output".to_string())
                .or_insert(Value::Null);
            if matches!(self.payload, RunEventPayload::RunFailed { .. }) {
                self.extra_fields
                    .entry("status".to_string())
                    .or_insert_with(|| Value::String("failed".to_string()));
            }
        }
        if let Some(budget_exhaustion) = budget_exhaustion {
            self.extra_fields.insert(
                "budget_exhaustion".to_string(),
                serde_json::to_value(budget_exhaustion).expect("budget exhaustion serializes"),
            );
        }
        self
    }

    pub fn final_output(&self) -> Option<&str> {
        self.extra_fields
            .get("final_output")
            .and_then(Value::as_str)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunEventPayload {
    RunStarted {
        input: String,
    },
    RunStateChanged {
        state: String,
    },
    AgentStarted,
    CycleStarted,
    LlmStarted {
        model: String,
    },
    AssistantDelta {
        delta: String,
    },
    ToolCallStarted {
        tool_call_id: String,
        tool_name: String,
        arguments: Value,
    },
    ApprovalRequested {
        request_id: String,
        tool_call_id: String,
        tool_name: String,
        #[serde(alias = "preview")]
        message: String,
    },
    ApprovalResolved {
        request_id: String,
        tool_name: String,
        tool_call_id: String,
        #[serde(default)]
        approved: bool,
    },
    ToolCallCompleted {
        tool_call_id: String,
        tool_name: String,
        status: ToolStatus,
    },
    MemoryCompactStarted {
        message_count: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        estimated_tokens: Option<u64>,
    },
    MemoryCompactCompleted {
        before_count: usize,
        after_count: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary_tokens: Option<u64>,
    },
    SubRunStarted {
        parent_tool_call_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        child_session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
    },
    SubRunCompleted {
        parent_tool_call_id: String,
        status: AgentStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        final_output: Option<String>,
    },
    Handoff {
        source_agent: String,
        target_agent: String,
        tool_call_id: String,
    },
    HandoffStarted {
        source_agent: String,
        target_agent: String,
        tool_call_id: String,
    },
    HandoffCompleted {
        source_agent: String,
        target_agent: String,
        tool_call_id: String,
    },
    SessionPersisted,
    BudgetSnapshot {
        enforcement_boundary: BudgetEnforcementBoundary,
        budget_usage: BudgetUsageSnapshot,
    },
    BudgetExhausted {
        enforcement_boundary: BudgetEnforcementBoundary,
        budget_usage: BudgetUsageSnapshot,
        budget_exhaustion: BudgetExhaustion,
    },
    CheckpointCreated {
        checkpoint_key: String,
        resume_attempt: u64,
    },
    CheckpointResumed {
        checkpoint_key: String,
        resume_attempt: u64,
    },
    OperationReplayed {
        checkpoint_key: String,
        operation_id: String,
        operation_kind: OperationKind,
        receipt_state: OperationState,
    },
    OperationAmbiguous {
        checkpoint_key: String,
        operation_id: String,
        operation_kind: OperationKind,
        risk: String,
        idempotency_support: Option<ToolIdempotency>,
    },
    ReconciliationRequired {
        checkpoint_key: String,
        operation_id: String,
        operation_kind: OperationKind,
        interruption_reason: String,
        resume_observation: ResumeObservation,
    },
    ModelRetryDuplicateRisk {
        checkpoint_key: String,
        operation_id: String,
        operation_kind: OperationKind,
        risk: String,
    },
    ReconciliationResolved {
        checkpoint_key: String,
        operation_id: String,
        operation_kind: OperationKind,
        decision: ReconciliationDecisionKind,
    },
    RunCompleted {
        status: AgentStatus,
    },
    RunFailed {
        error: String,
    },
    RunCancelled {
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalAction {
    #[default]
    Allow,
    AllowSession,
    Deny,
    Timeout,
}

impl ApprovalAction {
    pub fn from_approved(approved: bool) -> Self {
        if approved {
            Self::Allow
        } else {
            Self::Deny
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "allow" => Some(Self::Allow),
            "allow_session" => Some(Self::AllowSession),
            "deny" => Some(Self::Deny),
            "timeout" => Some(Self::Timeout),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::AllowSession => "allow_session",
            Self::Deny => "deny",
            Self::Timeout => "timeout",
        }
    }

    pub fn is_approved(self) -> bool {
        matches!(self, Self::Allow | Self::AllowSession)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Started,
    Success,
    Error,
    WaitResponse,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentErrorPayload {
    pub message: String,
    pub code: Option<String>,
}

impl AgentErrorPayload {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: None,
        }
    }
}

fn timestamp_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_micros() as f64 / 1_000_000.0)
        .unwrap_or_default()
}
