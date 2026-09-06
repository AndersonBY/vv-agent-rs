use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::budget::{BudgetExhaustion, BudgetUsageSnapshot};
use crate::checkpoint::ResumeObservation;

use super::{
    AgentStatus, CompletionReason, LLMResponse, Message, Metadata, TaskTokenUsage, ToolCall,
    ToolExecutionResult,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CycleRecord {
    pub index: u32,
    pub assistant_message: String,
    pub tool_calls: Vec<ToolCall>,
    pub tool_results: Vec<ToolExecutionResult>,
    pub memory_compacted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentResultError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

impl AgentResultError {
    pub fn new(code: impl Into<String>, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.code.trim().is_empty() || self.message.trim().is_empty() {
            return Err("AgentResult error code and message must be non-empty".to_string());
        }
        Ok(())
    }
}

impl fmt::Display for AgentResultError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(formatter)
    }
}

impl CycleRecord {
    pub fn from_response(
        index: u32,
        response: &LLMResponse,
        tool_results: Vec<ToolExecutionResult>,
    ) -> Self {
        Self {
            index,
            assistant_message: response.content.clone(),
            tool_calls: response.tool_calls.clone(),
            tool_results,
            memory_compacted: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentResult {
    pub status: AgentStatus,
    pub messages: Vec<Message>,
    pub cycles: Vec<CycleRecord>,
    #[serde(default)]
    pub completion_reason: Option<CompletionReason>,
    #[serde(default)]
    pub completion_tool_name: Option<String>,
    #[serde(default)]
    pub partial_output: Option<String>,
    #[serde(default)]
    pub budget_usage: Option<BudgetUsageSnapshot>,
    #[serde(default)]
    pub budget_exhaustion: Option<BudgetExhaustion>,
    #[serde(default)]
    pub checkpoint_key: Option<String>,
    pub resume_observations: Vec<ResumeObservation>,
    pub final_answer: Option<String>,
    pub wait_reason: Option<String>,
    pub error: Option<AgentResultError>,
    #[serde(default)]
    pub error_code: Option<String>,
    pub shared_state: Metadata,
    pub token_usage: TaskTokenUsage,
}

impl Default for AgentResult {
    fn default() -> Self {
        Self {
            status: AgentStatus::Pending,
            messages: Vec::new(),
            cycles: Vec::new(),
            completion_reason: None,
            completion_tool_name: None,
            partial_output: None,
            budget_usage: None,
            budget_exhaustion: None,
            checkpoint_key: None,
            resume_observations: Vec::new(),
            final_answer: None,
            wait_reason: None,
            error: None,
            error_code: None,
            shared_state: Metadata::new(),
            token_usage: TaskTokenUsage::default(),
        }
    }
}

impl AgentResult {
    pub fn completed(
        messages: Vec<Message>,
        cycles: Vec<CycleRecord>,
        final_answer: impl Into<String>,
    ) -> Self {
        Self::completed_with_shared_state(messages, cycles, final_answer, Metadata::new())
    }

    pub fn completed_with_shared_state(
        messages: Vec<Message>,
        cycles: Vec<CycleRecord>,
        final_answer: impl Into<String>,
        shared_state: Metadata,
    ) -> Self {
        Self {
            status: AgentStatus::Completed,
            messages,
            cycles,
            completion_reason: Some(CompletionReason::ToolFinish),
            completion_tool_name: None,
            partial_output: None,
            budget_usage: None,
            budget_exhaustion: None,
            checkpoint_key: None,
            resume_observations: Vec::new(),
            final_answer: Some(final_answer.into()),
            wait_reason: None,
            error: None,
            error_code: None,
            shared_state,
            token_usage: TaskTokenUsage::default(),
        }
    }

    pub fn failed(error: impl Into<String>) -> Self {
        Self::failed_with_code("agent_failed", error, false)
    }

    pub fn failed_with_code(
        code: impl Into<String>,
        error: impl Into<String>,
        retryable: bool,
    ) -> Self {
        Self {
            status: AgentStatus::Failed,
            messages: Vec::new(),
            cycles: Vec::new(),
            completion_reason: Some(CompletionReason::Failed),
            completion_tool_name: None,
            partial_output: None,
            budget_usage: None,
            budget_exhaustion: None,
            checkpoint_key: None,
            resume_observations: Vec::new(),
            final_answer: None,
            wait_reason: None,
            error: Some(AgentResultError::new(code, error, retryable)),
            error_code: None,
            shared_state: Metadata::new(),
            token_usage: TaskTokenUsage::default(),
        }
    }

    pub fn todo_list(&self) -> Vec<Value> {
        self.shared_state
            .get("todo_list")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }
}

pub(crate) fn last_assistant_output(cycles: &[CycleRecord]) -> Option<String> {
    cycles
        .iter()
        .rev()
        .find(|cycle| !cycle.assistant_message.trim().is_empty())
        .map(|cycle| cycle.assistant_message.clone())
}
