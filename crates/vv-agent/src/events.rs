use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::{AgentStatus, Metadata};

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
        static NEXT_EVENT_ID: AtomicU64 = AtomicU64::new(1);
        let sequence = NEXT_EVENT_ID.fetch_add(1, Ordering::Relaxed);
        Self(format!("evt_{}_{}", timestamp_millis(), sequence))
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunEvent {
    pub version: RunEventVersion,
    pub event_id: EventId,
    pub run_id: String,
    pub trace_id: String,
    pub session_id: Option<String>,
    pub parent_event_id: Option<String>,
    pub parent_run_id: Option<String>,
    pub created_at_ms: u128,
    pub cycle_index: Option<u32>,
    pub agent_name: Option<String>,
    pub metadata: Metadata,
    #[serde(flatten)]
    pub payload: RunEventPayload,
}

impl RunEvent {
    pub fn new(
        run_id: impl Into<String>,
        trace_id: impl Into<String>,
        agent_name: impl Into<String>,
        cycle_index: Option<u32>,
        payload: RunEventPayload,
    ) -> Self {
        Self {
            version: RunEventVersion::v1(),
            event_id: EventId::new(),
            run_id: run_id.into(),
            trace_id: trace_id.into(),
            session_id: None,
            parent_event_id: None,
            parent_run_id: None,
            created_at_ms: timestamp_millis(),
            cycle_index,
            agent_name: Some(agent_name.into()),
            metadata: Metadata::new(),
            payload,
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
        preview: impl Into<String>,
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
                preview: preview.into(),
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
        Self::new(
            run_id,
            trace_id,
            agent_name,
            None,
            RunEventPayload::RunFailed { error },
        )
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

    pub fn created_at_ms(&self) -> u128 {
        self.created_at_ms
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
        preview: String,
    },
    ApprovalResolved {
        request_id: String,
        tool_call_id: String,
        tool_name: String,
        approved: bool,
    },
    ToolCallCompleted {
        tool_call_id: String,
        tool_name: String,
        status: ToolStatus,
    },
    MemoryCompactStarted {
        message_count: usize,
        estimated_tokens: Option<u64>,
    },
    MemoryCompactCompleted {
        before_count: usize,
        after_count: usize,
        summary_tokens: Option<u64>,
    },
    SubRunStarted {
        parent_tool_call_id: String,
        child_session_id: Option<String>,
        task_id: Option<String>,
    },
    SubRunCompleted {
        parent_tool_call_id: String,
        status: AgentStatus,
        final_output: Option<String>,
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
    SessionPersisted {
        session_id: String,
    },
    RunCompleted {
        status: AgentStatus,
    },
    RunFailed {
        error: AgentErrorPayload,
    },
    RunCancelled {
        reason: String,
    },
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

fn timestamp_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}
