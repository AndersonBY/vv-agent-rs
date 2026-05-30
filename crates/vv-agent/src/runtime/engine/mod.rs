mod completion;
mod construction;
mod controls;
mod cycle_inputs;
mod helpers;
mod logging;
mod memory;
mod planning;
mod run_setup;
mod state;

use std::collections::BTreeMap;

use serde_json::Value;

use crate::llm::{LlmClient, LlmError, LlmRequest};
use crate::memory::CompactionExhaustedError;
use crate::tools::ToolContext;
use crate::types::{AgentResult, AgentStatus, AgentTask, ToolDirective, ToolExecutionResult};

use super::cancellation::CancellationToken;

use super::cycle_runner::{is_prompt_too_long_error, MAX_PROMPT_TOO_LONG_RETRIES};
use super::results::assistant_message_from_response;
use super::tool_call_runner::{execute_tool_result, needs_tool_call_id, skipped_tool_result};

use self::completion::{handle_directive_result, handle_no_tool_response, NoToolResponseRequest};
use self::helpers::{
    cancelled_agent_result, collect_interruption_messages, controls_cancelled,
    drain_steering_queue, failed_agent_result, image_notification_from_tool_result,
    previous_cycle_memory_usage,
};
use self::run_setup::{prepare_run_setup, PreparedRun};
pub use self::state::AgentRuntime;

pub use crate::runtime::sub_agent_sessions::{
    _register_sub_agent_session, _unregister_sub_agent_session, get_sub_agent_session,
    steer_sub_agent_session, subscribe_sub_agent_session,
};
pub use controls::{
    BeforeCycleMessageProvider, InterruptionMessageProvider, RuntimeEventHandler,
    RuntimeLogCallback, RuntimeLogHandler, RuntimeRunControls,
};

impl<C: LlmClient + Clone + 'static> AgentRuntime<C> {
    pub fn run(&self, task: AgentTask) -> Result<AgentResult, LlmError> {
        self.run_with_controls(task, RuntimeRunControls::default())
    }

    pub fn run_with_controls(
        &self,
        task: AgentTask,
        controls: RuntimeRunControls,
    ) -> Result<AgentResult, LlmError> {
        let PreparedRun {
            task,
            messages,
            cycles,
            shared_state,
            workspace_path,
            workspace_backend,
            sub_task_manager,
            mut memory_manager,
        } = prepare_run_setup(self, task, &controls);
        self.emit_run_started(&controls, &task, &workspace_path);

        if controls_cancelled(&controls) {
            self.emit_log(
                &controls,
                "run_cancelled",
                BTreeMap::from([(
                    "error".to_string(),
                    Value::String("Operation was cancelled".to_string()),
                )]),
            );
            return Ok(cancelled_agent_result(messages, cycles, shared_state));
        }

        let effective_cancellation_token = controls.effective_cancellation_token();
        let effective_stream_callback = controls.effective_stream_callback();
        let mut pending_error = None;
        let result = self.execution_backend.execute(
            &task,
            messages,
            shared_state,
            |cycle_index, messages, cycles, shared_state, cancellation_token| {
                if cancellation_token.is_some_and(CancellationToken::is_cancelled)
                    || controls_cancelled(&controls)
                {
                    self.emit_log(
                        &controls,
                        "run_cancelled",
                        BTreeMap::from([
                            ("cycle".to_string(), Value::from(cycle_index)),
                            (
                                "error".to_string(),
                                Value::String("Operation was cancelled".to_string()),
                            ),
                        ]),
                    );
                    return Some(cancelled_agent_result(
                        messages.clone(),
                        cycles.clone(),
                        shared_state.clone(),
                    ));
                }
                self.apply_cycle_inputs(&controls, cycle_index, messages, shared_state);
                self.emit_log(
                    &controls,
                    "cycle_started",
                    BTreeMap::from([
                        ("cycle".to_string(), Value::from(cycle_index)),
                        ("max_cycles".to_string(), Value::from(task.max_cycles)),
                        ("message_count".to_string(), Value::from(messages.len())),
                    ]),
                );
                let hook_manager = self.hook_manager();
                let pre_compact_messages = hook_manager.apply_before_memory_compact(
                    &task,
                    cycle_index,
                    messages.clone(),
                    shared_state,
                );
                let pre_compact_messages =
                    memory_manager.apply_session_memory_context(&pre_compact_messages);
                let (previous_prompt_tokens, recent_tool_call_ids) =
                    previous_cycle_memory_usage(cycles);
                let (prepared_messages, memory_compacted) = memory_manager
                    .compact_for_cycle_with_usage(
                        &pre_compact_messages,
                        cycle_index,
                        false,
                        previous_prompt_tokens,
                        recent_tool_call_ids.as_ref(),
                    );
                *messages = prepared_messages;
                let tool_schemas = self.planned_tool_schemas(&task);
                let llm_messages = memory_manager.apply_session_memory_context(messages);
                let (request_messages, request_tool_schemas) = hook_manager.apply_before_llm(
                    &task,
                    cycle_index,
                    llm_messages,
                    tool_schemas,
                    shared_state,
                );
                let mut request_messages = request_messages;
                let mut request_tool_schemas = request_tool_schemas;
                let mut memory_compacted = memory_compacted;
                let mut prompt_too_long_retries = 0;
                let response = loop {
                    let mut request = LlmRequest::new(task.model.clone(), request_messages.clone());
                    request.tools = request_tool_schemas.clone();
                    match self
                        .llm_client
                        .complete_with_stream(request, effective_stream_callback.clone())
                    {
                        Ok(response) => break response,
                        Err(error) if is_prompt_too_long_error(&error) => {
                            prompt_too_long_retries += 1;
                            if prompt_too_long_retries > MAX_PROMPT_TOO_LONG_RETRIES {
                                let error =
                                    LlmError::CompactionExhausted(CompactionExhaustedError::new(
                                        prompt_too_long_retries,
                                        Some(error.to_string()),
                                    ));
                                let message = error.to_string();
                                pending_error = Some(error);
                                return Some(failed_agent_result(
                                    messages.clone(),
                                    cycles.clone(),
                                    shared_state.clone(),
                                    message,
                                ));
                            }
                            memory_compacted = true;
                            let retry_messages = if prompt_too_long_retries == 1 {
                                let (compacted, _) = memory_manager.compact_for_cycle(
                                    &request_messages,
                                    cycle_index,
                                    true,
                                );
                                compacted
                            } else {
                                memory_manager.emergency_compact(
                                    &request_messages,
                                    (0.2 * prompt_too_long_retries as f64).min(0.95),
                                )
                            };
                            let retry_tool_schemas = self.planned_tool_schemas(&task);
                            let llm_messages =
                                memory_manager.apply_session_memory_context(&retry_messages);
                            (request_messages, request_tool_schemas) = hook_manager
                                .apply_before_llm(
                                    &task,
                                    cycle_index,
                                    llm_messages,
                                    retry_tool_schemas,
                                    shared_state,
                                );
                        }
                        Err(error) => {
                            let message = error.to_string();
                            pending_error = Some(error);
                            return Some(failed_agent_result(
                                messages.clone(),
                                cycles.clone(),
                                shared_state.clone(),
                                message,
                            ));
                        }
                    }
                };
                let response = hook_manager.apply_after_llm(
                    &task,
                    cycle_index,
                    &request_messages,
                    &request_tool_schemas,
                    response,
                    shared_state,
                );
                *messages = request_messages;
                messages.push(assistant_message_from_response(&response));
                let mut cycle = crate::types::CycleRecord::from_response(
                    cycle_index,
                    &response,
                    Vec::<ToolExecutionResult>::new(),
                );
                cycle.memory_compacted = memory_compacted;
                self.emit_cycle_llm_response(&controls, &cycle);

                if response.tool_calls.is_empty() {
                    if let Some(result) = handle_no_tool_response(NoToolResponseRequest {
                        runtime: self,
                        controls: &controls,
                        task: &task,
                        cycle_index,
                        response: &response,
                        messages,
                        cycles,
                        cycle,
                        shared_state,
                    }) {
                        return Some(result);
                    }
                    return None;
                }

                let sub_task_runner = self.build_sub_task_runner(
                    &task,
                    workspace_path.clone(),
                    workspace_backend.clone(),
                    shared_state.clone(),
                    sub_task_manager.clone(),
                    super::sub_agents::SubTaskCallbacks {
                        stream_callback: effective_stream_callback.clone(),
                        parent_log_handler: self.log_handler.clone(),
                        parent_event_handler: controls.log_handler.clone(),
                    },
                );
                let mut tool_metadata = controls
                    .execution_context
                    .as_ref()
                    .map(|context| context.metadata.clone())
                    .unwrap_or_default();
                tool_metadata.extend(task.metadata.clone());
                let mut context = ToolContext {
                    workspace: workspace_path.clone(),
                    shared_state: shared_state.clone(),
                    cycle_index,
                    task_id: task.task_id.clone(),
                    metadata: tool_metadata,
                    workspace_backend: workspace_backend.clone(),
                    sub_task_runner,
                    sub_task_manager: Some(sub_task_manager.clone()),
                    execution_backend: Some(self.execution_backend.clone()),
                };

                let mut directive_result = None;
                let mut image_notifications = Vec::new();
                for (call_index, call) in response.tool_calls.iter().enumerate() {
                    if cancellation_token.is_some_and(CancellationToken::is_cancelled)
                        || controls_cancelled(&controls)
                    {
                        *shared_state = context.shared_state.clone();
                        cycles.push(cycle);
                        self.emit_log(
                            &controls,
                            "run_cancelled",
                            BTreeMap::from([
                                ("cycle".to_string(), Value::from(cycle_index)),
                                (
                                    "error".to_string(),
                                    Value::String("Operation was cancelled".to_string()),
                                ),
                            ]),
                        );
                        return Some(cancelled_agent_result(
                            messages.clone(),
                            cycles.clone(),
                            shared_state.clone(),
                        ));
                    }
                    let (patched_call, short_circuit_result) = hook_manager.apply_before_tool_call(
                        &task,
                        cycle_index,
                        call.clone(),
                        &context,
                    );
                    let mut result = match short_circuit_result {
                        Some(mut result) => {
                            if needs_tool_call_id(&result.tool_call_id) {
                                result.tool_call_id = call.id.clone();
                            }
                            result
                        }
                        None => {
                            let mut result = execute_tool_result(
                                &self.tool_registry,
                                &patched_call,
                                &mut context,
                            );
                            if needs_tool_call_id(&result.tool_call_id) {
                                result.tool_call_id = patched_call.id.clone();
                            }
                            result
                        }
                    };
                    result = hook_manager.apply_after_tool_call(
                        &task,
                        cycle_index,
                        &patched_call,
                        &context,
                        result,
                    );
                    if needs_tool_call_id(&result.tool_call_id) {
                        result.tool_call_id = patched_call.id.clone();
                    }
                    self.emit_tool_result(&controls, cycle_index, &patched_call, &result);

                    let interruption_messages = collect_interruption_messages(&controls);
                    let steering_prompts = drain_steering_queue(&controls);
                    let steering_count = interruption_messages.len() + steering_prompts.len();
                    if steering_count == 0 && result.directive != ToolDirective::Continue {
                        directive_result = Some(result.clone());
                    }
                    messages.push(result.to_message());
                    if let Some(image_notification) =
                        image_notification_from_tool_result(&result, task.native_multimodal)
                    {
                        image_notifications.push(image_notification);
                    }
                    cycle.tool_results.push(result);
                    if steering_count > 0 {
                        for prompt in &steering_prompts {
                            self.emit_log(
                                &controls,
                                "session_steer_interrupt",
                                BTreeMap::from([
                                    ("cycle".to_string(), Value::from(cycle_index)),
                                    (
                                        "after_tool_call_id".to_string(),
                                        Value::String(patched_call.id.clone()),
                                    ),
                                    (
                                        "after_tool_name".to_string(),
                                        Value::String(patched_call.name.clone()),
                                    ),
                                    ("prompt".to_string(), Value::String(prompt.clone())),
                                ]),
                            );
                        }
                        for skipped_call in response.tool_calls.iter().skip(call_index + 1) {
                            let skipped = skipped_tool_result(
                                skipped_call,
                                "skipped_due_to_steering",
                                "Tool skipped due to queued steering message.",
                            );
                            self.emit_tool_result(&controls, cycle_index, skipped_call, &skipped);
                            messages.push(skipped.to_message());
                            cycle.tool_results.push(skipped);
                        }
                        for prompt in &steering_prompts {
                            messages.push(crate::types::Message::user(prompt.clone()));
                        }
                        messages.extend(interruption_messages);
                        self.emit_log(
                            &controls,
                            "run_steered",
                            BTreeMap::from([
                                ("cycle".to_string(), Value::from(cycle_index)),
                                (
                                    "after_tool_call_id".to_string(),
                                    Value::String(patched_call.id.clone()),
                                ),
                                (
                                    "after_tool_name".to_string(),
                                    Value::String(patched_call.name.clone()),
                                ),
                                (
                                    "prompt_count".to_string(),
                                    Value::from(steering_count as u64),
                                ),
                                (
                                    "steering_count".to_string(),
                                    Value::from(steering_count as u64),
                                ),
                            ]),
                        );
                        break;
                    }
                    if directive_result.is_some() {
                        let (error_code, message) = match directive_result
                            .as_ref()
                            .map(|result| result.directive)
                            .unwrap_or(ToolDirective::Continue)
                        {
                            ToolDirective::WaitUser => (
                                "skipped_due_to_wait_user",
                                "Tool skipped because a previous tool requested user input.",
                            ),
                            ToolDirective::Finish => (
                                "skipped_due_to_finish",
                                "Tool skipped because a previous tool finished the task.",
                            ),
                            ToolDirective::Continue => {
                                ("skipped_due_to_directive", "Tool skipped.")
                            }
                        };
                        for skipped_call in response.tool_calls.iter().skip(call_index + 1) {
                            let skipped = skipped_tool_result(skipped_call, error_code, message);
                            self.emit_tool_result(&controls, cycle_index, skipped_call, &skipped);
                            messages.push(skipped.to_message());
                            cycle.tool_results.push(skipped);
                        }
                        break;
                    }
                }
                messages.extend(image_notifications);
                *shared_state = context.shared_state.clone();

                cycles.push(cycle);
                if let Some(directive_result) = directive_result.as_ref() {
                    if let Some(result) = handle_directive_result(
                        self,
                        &controls,
                        cycle_index,
                        directive_result,
                        messages,
                        cycles,
                        shared_state,
                    ) {
                        return Some(result);
                    }
                }
                None
            },
            effective_cancellation_token.as_ref(),
            task.max_cycles,
        );
        if let Some(error) = pending_error {
            return Err(error);
        }
        if result.status == AgentStatus::MaxCycles {
            self.emit_run_max_cycles(&controls, &result);
        }
        Ok(result)
    }
}
