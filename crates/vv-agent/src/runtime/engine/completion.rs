use std::collections::BTreeMap;

use serde_json::Value;

use crate::llm::LlmClient;
use crate::runtime::token_usage::summarize_task_token_usage;
use crate::types::{
    AgentResult, AgentStatus, AgentTask, CycleRecord, LLMResponse, Message, NoToolPolicy,
    ToolDirective, ToolExecutionResult,
};

use super::super::results::{extract_final_message, extract_wait_reason};
use super::{AgentRuntime, RuntimeRunControls};

pub(super) struct NoToolResponseRequest<'a, C: LlmClient> {
    pub runtime: &'a AgentRuntime<C>,
    pub controls: &'a RuntimeRunControls,
    pub task: &'a AgentTask,
    pub cycle_index: u32,
    pub response: &'a LLMResponse,
    pub messages: &'a mut Vec<Message>,
    pub cycles: &'a mut Vec<CycleRecord>,
    pub cycle: CycleRecord,
    pub shared_state: &'a BTreeMap<String, Value>,
}

pub(super) fn handle_no_tool_response<C: LlmClient>(
    request: NoToolResponseRequest<'_, C>,
) -> Option<AgentResult> {
    let NoToolResponseRequest {
        runtime,
        controls,
        task,
        cycle_index,
        response,
        messages,
        cycles,
        cycle,
        shared_state,
    } = request;
    cycles.push(cycle);
    match task.no_tool_policy {
        NoToolPolicy::Finish => {
            runtime.emit_log(
                controls,
                "run_completed",
                BTreeMap::from([
                    ("task_id".to_string(), Value::String(task.task_id.clone())),
                    ("cycle".to_string(), Value::from(cycle_index)),
                    (
                        "final_answer".to_string(),
                        Value::String(runtime.preview_text(&response.content)),
                    ),
                ]),
            );
            Some(AgentResult::completed_with_shared_state(
                messages.clone(),
                cycles.clone(),
                response.content.clone(),
                shared_state.clone(),
            ))
        }
        NoToolPolicy::WaitUser => {
            let wait_reason = if response.content.is_empty() {
                "No tool call and runtime is waiting for user.".to_string()
            } else {
                response.content.clone()
            };
            runtime.emit_log(
                controls,
                "run_wait_user",
                BTreeMap::from([
                    ("cycle".to_string(), Value::from(cycle_index)),
                    (
                        "wait_reason".to_string(),
                        Value::String(runtime.preview_text(&wait_reason)),
                    ),
                ]),
            );
            Some(AgentResult {
                status: AgentStatus::WaitUser,
                messages: messages.clone(),
                cycles: cycles.clone(),
                final_answer: None,
                wait_reason: Some(wait_reason),
                error: None,
                shared_state: shared_state.clone(),
                token_usage: summarize_task_token_usage(cycles),
            })
        }
        NoToolPolicy::Continue => {
            messages.push(Message::user(
                "Continue. If the task is complete, call task_finish.",
            ));
            None
        }
    }
}

pub(super) fn handle_directive_result<C: LlmClient>(
    runtime: &AgentRuntime<C>,
    controls: &RuntimeRunControls,
    task: &AgentTask,
    cycle_index: u32,
    result: &ToolExecutionResult,
    messages: &[Message],
    cycles: &[CycleRecord],
    shared_state: &BTreeMap<String, Value>,
) -> Option<AgentResult> {
    match result.directive {
        ToolDirective::Finish => {
            let final_message = extract_final_message(result);
            runtime.emit_log(
                controls,
                "run_completed",
                BTreeMap::from([
                    ("task_id".to_string(), Value::String(task.task_id.clone())),
                    ("cycle".to_string(), Value::from(cycle_index)),
                    (
                        "final_answer".to_string(),
                        Value::String(runtime.preview_text(&final_message)),
                    ),
                ]),
            );
            Some(AgentResult::completed_with_shared_state(
                messages.to_vec(),
                cycles.to_vec(),
                final_message,
                shared_state.clone(),
            ))
        }
        ToolDirective::WaitUser => {
            let wait_reason = extract_wait_reason(result);
            runtime.emit_log(
                controls,
                "run_wait_user",
                BTreeMap::from([
                    ("cycle".to_string(), Value::from(cycle_index)),
                    (
                        "wait_reason".to_string(),
                        Value::String(runtime.preview_text(&wait_reason)),
                    ),
                ]),
            );
            Some(AgentResult {
                status: AgentStatus::WaitUser,
                messages: messages.to_vec(),
                cycles: cycles.to_vec(),
                final_answer: None,
                wait_reason: Some(wait_reason),
                error: None,
                shared_state: shared_state.clone(),
                token_usage: summarize_task_token_usage(cycles),
            })
        }
        ToolDirective::Continue => None,
    }
}
