use serde_json::Value;

use crate::types::{
    last_assistant_output, AgentResult, AgentStatus, CompletionReason, CycleRecord, LLMResponse,
    Message, Metadata, TaskTokenUsage, ToolExecutionResult,
};

pub(super) fn assistant_message_from_response(response: &LLMResponse) -> Message {
    let mut message = Message::assistant(response.content.clone());
    message.tool_calls = response.tool_calls.clone();
    message.reasoning_content = response
        .raw
        .get("reasoning_content")
        .and_then(Value::as_str)
        .filter(|reasoning| !reasoning.is_empty())
        .map(str::to_string);
    message
}

pub(crate) fn extract_final_message(result: &ToolExecutionResult) -> String {
    result
        .metadata
        .get("final_message")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            serde_json::from_str::<Value>(&result.content)
                .ok()
                .and_then(|value| {
                    value
                        .get("message")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
        })
        .unwrap_or_else(|| result.content.clone())
}

pub(crate) fn extract_wait_reason(result: &ToolExecutionResult) -> String {
    result
        .metadata
        .get("question")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| result.content.clone())
}

pub(crate) fn cancelled_agent_result(
    messages: Vec<Message>,
    cycles: Vec<CycleRecord>,
    shared_state: Metadata,
    token_usage: TaskTokenUsage,
) -> AgentResult {
    let partial_output = last_assistant_output(&cycles);
    AgentResult {
        status: AgentStatus::Failed,
        messages,
        cycles,
        completion_reason: Some(CompletionReason::Cancelled),
        completion_tool_name: None,
        partial_output,
        budget_usage: None,
        budget_exhaustion: None,
        checkpoint_key: None,
        resume_observations: Vec::new(),
        final_answer: None,
        wait_reason: None,
        error: Some(crate::types::AgentResultError::new(
            "cancelled",
            "Operation was cancelled",
            false,
        )),
        error_code: None,
        shared_state,
        token_usage,
    }
}
