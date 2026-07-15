use std::time::{SystemTime, UNIX_EPOCH};

use serde::{de::Error as _, Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::types::{AgentStatus, CompletionReason, Metadata};

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
}

impl Default for EventId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunEvent {
    pub version: RunEventVersion,
    pub event_id: EventId,
    pub run_id: String,
    pub trace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    pub created_at: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cycle_index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    #[serde(default, skip_serializing_if = "Metadata::is_empty")]
    pub metadata: Metadata,
    #[serde(flatten)]
    pub payload: RunEventPayload,
    #[serde(flatten, skip_serializing_if = "Metadata::is_empty")]
    extra_fields: Metadata,
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

fn validate_completion_wire_fields(value: &Value) -> Result<(), String> {
    if let Some(value) = value.get("completion_reason") {
        match value {
            Value::Null => {}
            Value::String(reason) if CompletionReason::parse(reason).is_some() => {}
            Value::String(reason) => {
                return Err(format!(
                    "unsupported run event completion_reason `{reason}`"
                ));
            }
            _ => return Err("run event completion_reason must be a string or null".to_string()),
        }
    }
    for field in ["completion_tool_name", "partial_output"] {
        if value
            .get(field)
            .is_some_and(|value| !value.is_null() && !value.is_string())
        {
            return Err(format!("run event {field} must be a string or null"));
        }
    }
    Ok(())
}

fn supplemental_wire_fields(value: &Value, payload: &RunEventPayload) -> Metadata {
    let Some(object) = value.as_object() else {
        return Metadata::new();
    };
    let keys: &[&str] = match payload {
        RunEventPayload::ApprovalResolved { .. } => &["action"],
        RunEventPayload::SubRunStarted { .. } => &["status"],
        RunEventPayload::SubRunCompleted { .. } => &[
            "child_session_id",
            "task_id",
            "wait_reason",
            "error",
            "completion_reason",
            "completion_tool_name",
            "partial_output",
            "token_usage",
        ],
        RunEventPayload::HandoffStarted { .. } => &["status", "child_session_id"],
        RunEventPayload::HandoffCompleted { .. } => &["status", "child_session_id", "child_run_id"],
        RunEventPayload::RunCompleted { .. } => &[
            "final_output",
            "completion_reason",
            "completion_tool_name",
            "partial_output",
        ],
        RunEventPayload::RunFailed { .. } | RunEventPayload::RunCancelled { .. } => {
            &["completion_reason", "partial_output"]
        }
        _ => &[],
    };
    keys.iter()
        .filter_map(|key| {
            object
                .get(*key)
                .cloned()
                .map(|value| ((*key).to_string(), value))
        })
        .collect()
}

fn add_default_supplemental_fields(payload: &RunEventPayload, fields: &mut Metadata) {
    let (key, value) = match payload {
        RunEventPayload::ApprovalResolved { approved, .. } => (
            "action",
            Value::String(
                ApprovalAction::from_approved(*approved)
                    .as_str()
                    .to_string(),
            ),
        ),
        RunEventPayload::SubRunStarted { .. } => ("status", Value::String("running".to_string())),
        RunEventPayload::HandoffStarted { .. } => ("status", Value::String("started".to_string())),
        RunEventPayload::HandoffCompleted { .. } => {
            ("status", Value::String("completed".to_string()))
        }
        RunEventPayload::RunCompleted { .. } => ("final_output", Value::Null),
        _ => return,
    };
    fields.entry(key.to_string()).or_insert(value);
}
