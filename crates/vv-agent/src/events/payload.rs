use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::budget::{BudgetEnforcementBoundary, BudgetExhaustion, BudgetUsageSnapshot};
use crate::checkpoint::{
    OperationKind, OperationState, ReconciliationDecisionKind, ResumeObservation, ToolIdempotency,
};
use crate::types::AgentStatus;

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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content_chars: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        estimated_tokens: Option<u64>,
    },
    ReasoningDelta {
        delta: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_chars: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        estimated_tokens: Option<u64>,
    },
    ModelToolCallStarted {
        tool_call_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_call_index: Option<u64>,
        tool_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        arguments_chars: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        estimated_tokens: Option<u64>,
    },
    ModelToolCallProgress {
        tool_call_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_call_index: Option<u64>,
        tool_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        arguments_chars: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        estimated_tokens: Option<u64>,
    },
    ToolCallPlanned {
        tool_call_id: String,
        tool_name: String,
        arguments: Value,
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
    Running,
    PendingCompress,
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
