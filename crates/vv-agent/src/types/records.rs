use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::budget::{BudgetExhaustion, BudgetUsageSnapshot};

use super::{
    AgentStatus, CompletionReason, LLMResponse, Message, Metadata, TaskTokenUsage, TokenUsage,
    ToolCall, ToolExecutionResult,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CycleRecord {
    pub index: u32,
    pub assistant_message: String,
    pub tool_calls: Vec<ToolCall>,
    pub tool_results: Vec<ToolExecutionResult>,
    pub memory_compacted: bool,
    pub token_usage: TokenUsage,
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
            token_usage: response.token_usage.clone(),
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
    pub final_answer: Option<String>,
    pub wait_reason: Option<String>,
    pub error: Option<String>,
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
            final_answer: None,
            wait_reason: None,
            error: None,
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
        let mut token_usage = TaskTokenUsage::default();
        for cycle in &cycles {
            token_usage.add_cycle(cycle.index, cycle.token_usage.clone());
        }
        Self {
            status: AgentStatus::Completed,
            messages,
            cycles,
            completion_reason: Some(CompletionReason::ToolFinish),
            completion_tool_name: None,
            partial_output: None,
            budget_usage: None,
            budget_exhaustion: None,
            final_answer: Some(final_answer.into()),
            wait_reason: None,
            error: None,
            shared_state,
            token_usage,
        }
    }

    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            status: AgentStatus::Failed,
            messages: Vec::new(),
            cycles: Vec::new(),
            completion_reason: Some(CompletionReason::Failed),
            completion_tool_name: None,
            partial_output: None,
            budget_usage: None,
            budget_exhaustion: None,
            final_answer: None,
            wait_reason: None,
            error: Some(error.into()),
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
