use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::llm::LlmClient;
use crate::runtime::cancellation::CancellationToken;
use crate::runtime::token_usage::summarize_task_token_usage;
use crate::types::{
    last_assistant_output, AgentResult, AgentStatus, AgentTask, CompletionReason, CycleRecord,
    Message, MessageRole, ToolExecutionResult,
};

use super::{AgentRuntime, RuntimeRunControls};

pub(super) fn drain_steering_queue(controls: &RuntimeRunControls) -> Vec<String> {
    let Some(queue) = &controls.steering_queue else {
        return Vec::new();
    };
    let Ok(mut queue) = queue.lock() else {
        return Vec::new();
    };
    queue.drain(..).collect()
}

pub(super) fn collect_interruption_messages(controls: &RuntimeRunControls) -> Vec<Message> {
    controls
        .interruption_messages
        .as_ref()
        .map(|provider| provider())
        .unwrap_or_default()
}

pub(super) fn image_notification_from_tool_result(
    result: &ToolExecutionResult,
    include_image: bool,
) -> Option<Message> {
    if !include_image {
        return None;
    }
    if let Some(image_url) = &result.image_url {
        let content = result
            .image_path
            .as_deref()
            .map(|image_path| format!("[Image loaded] {image_path}"))
            .unwrap_or_default();
        let mut image_message = Message::user(content);
        image_message.image_url = Some(image_url.clone());
        image_message.metadata = result.metadata.clone();
        return Some(image_message);
    }
    result
        .image_path
        .as_deref()
        .map(|image_path| Message::user(format!("[Image loaded] {image_path}")))
}

pub(super) fn controls_cancelled(controls: &RuntimeRunControls) -> bool {
    controls
        .effective_cancellation_token()
        .as_ref()
        .is_some_and(CancellationToken::is_cancelled)
}

pub(super) fn project_cycle_cancellation<C: LlmClient>(
    runtime: &AgentRuntime<C>,
    controls: &RuntimeRunControls,
    cycle_index: u32,
    cancellation_token: Option<&CancellationToken>,
    messages: &[Message],
    cycles: &[CycleRecord],
    shared_state: &BTreeMap<String, Value>,
) -> Option<AgentResult> {
    if !cancellation_token.is_some_and(CancellationToken::is_cancelled)
        && !controls_cancelled(controls)
    {
        return None;
    }
    runtime.emit_log(
        controls,
        "run_cancelled",
        BTreeMap::from([
            ("cycle".to_string(), Value::from(cycle_index)),
            (
                "error".to_string(),
                Value::String("Operation was cancelled".to_string()),
            ),
        ]),
    );
    Some(cancelled_agent_result(
        messages.to_vec(),
        cycles.to_vec(),
        shared_state.clone(),
    ))
}

pub(super) fn seed_skill_state_from_task_metadata(
    shared_state: &mut BTreeMap<String, Value>,
    metadata: &BTreeMap<String, Value>,
) {
    if !shared_state.contains_key("available_skills") {
        if let Some(value) = metadata
            .get("available_skills")
            .filter(|value| !value.is_null())
        {
            shared_state.insert("available_skills".to_string(), value.clone());
        }
    }
    if !shared_state.contains_key("active_skills") {
        if let Some(value) = metadata
            .get("active_skills")
            .filter(|value| !value.is_null())
        {
            let value = value
                .as_array()
                .map(|items| Value::Array(items.clone()))
                .unwrap_or_else(|| Value::Array(Vec::new()));
            shared_state.insert("active_skills".to_string(), value);
        }
    }
}

pub(super) fn cancelled_agent_result(
    messages: Vec<Message>,
    cycles: Vec<CycleRecord>,
    shared_state: BTreeMap<String, Value>,
) -> AgentResult {
    let token_usage = summarize_task_token_usage(&cycles);
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
        resume_observation: None,
        final_answer: None,
        wait_reason: None,
        error: Some("Operation was cancelled".to_string()),
        shared_state,
        token_usage,
    }
}

pub(super) fn failed_agent_result(
    messages: Vec<Message>,
    cycles: Vec<CycleRecord>,
    shared_state: BTreeMap<String, Value>,
    error: String,
) -> AgentResult {
    let token_usage = summarize_task_token_usage(&cycles);
    let partial_output = last_assistant_output(&cycles);
    AgentResult {
        status: AgentStatus::Failed,
        messages,
        cycles,
        completion_reason: Some(CompletionReason::Failed),
        completion_tool_name: None,
        partial_output,
        budget_usage: None,
        budget_exhaustion: None,
        checkpoint_key: None,
        resume_observation: None,
        final_answer: None,
        wait_reason: None,
        error: Some(error),
        shared_state,
        token_usage,
    }
}

pub(super) fn finalize_terminal_projection<C: LlmClient>(
    runtime: &AgentRuntime<C>,
    controls: &RuntimeRunControls,
    cancellation_token: Option<&CancellationToken>,
    mut result: AgentResult,
) -> AgentResult {
    if result.status == AgentStatus::Failed
        && cancellation_token.is_some_and(CancellationToken::is_cancelled)
    {
        result.completion_reason = Some(CompletionReason::Cancelled);
        result.completion_tool_name = None;
        result.partial_output = result
            .partial_output
            .or_else(|| last_assistant_output(&result.cycles));
    } else if result.status == AgentStatus::MaxCycles {
        result.completion_reason = Some(CompletionReason::MaxCycles);
        result.completion_tool_name = None;
        result.partial_output = result
            .partial_output
            .or_else(|| last_assistant_output(&result.cycles));
    } else if result.status == AgentStatus::Failed && result.completion_reason.is_none() {
        result.completion_reason = Some(CompletionReason::Failed);
        result.partial_output = result
            .partial_output
            .or_else(|| last_assistant_output(&result.cycles));
    }
    if result.status == AgentStatus::MaxCycles {
        runtime.emit_run_max_cycles(controls, &result);
    }
    result
}

pub(crate) fn build_initial_messages(task: &AgentTask) -> Vec<Message> {
    if !task.initial_messages.is_empty() {
        let mut messages = task.initial_messages.clone();
        let starts_with_system = messages
            .first()
            .is_some_and(|message| message.role == MessageRole::System);
        if !starts_with_system && !task.system_prompt.is_empty() {
            messages.insert(0, system_message_from_task(task));
        } else if starts_with_system && !task.metadata.is_empty() {
            if let Some(system_message) = messages.first_mut() {
                let mut metadata = task.metadata.clone();
                metadata.extend(system_message.metadata.clone());
                if task.metadata.get("is_sub_task") == Some(&Value::Bool(true)) {
                    for key in crate::runtime::sub_agents::RESERVED_SUB_AGENT_METADATA_KEYS {
                        if let Some(value) = task.metadata.get(key) {
                            metadata.insert(key.to_string(), value.clone());
                        } else {
                            metadata.remove(key);
                        }
                    }
                }
                system_message.metadata = metadata;
            }
        }
        if !task.user_prompt.is_empty() {
            messages.push(Message::user(task.user_prompt.clone()));
        }
        return messages;
    }

    let mut messages = Vec::new();
    if !task.system_prompt.is_empty() {
        messages.push(system_message_from_task(task));
    }
    messages.push(Message::user(task.user_prompt.clone()));
    messages
}

fn system_message_from_task(task: &AgentTask) -> Message {
    let mut message = Message::system(task.system_prompt.clone());
    message.metadata = task.metadata.clone();
    message
}

pub(super) fn previous_cycle_memory_usage(
    cycles: &[CycleRecord],
) -> (Option<u64>, Option<BTreeSet<String>>) {
    let Some(last_cycle) = cycles.last() else {
        return (None, None);
    };
    let prompt_tokens = if last_cycle.token_usage.prompt_tokens > 0 {
        last_cycle.token_usage.prompt_tokens
    } else {
        last_cycle.token_usage.input_tokens
    };
    let recent_tool_call_ids = last_cycle
        .tool_calls
        .iter()
        .filter_map(|tool_call| {
            let tool_call_id = tool_call.id.trim();
            (!tool_call_id.is_empty()).then(|| tool_call_id.to_string())
        })
        .collect::<BTreeSet<_>>();
    (
        (prompt_tokens > 0).then_some(prompt_tokens),
        (!recent_tool_call_ids.is_empty()).then_some(recent_tool_call_ids),
    )
}
