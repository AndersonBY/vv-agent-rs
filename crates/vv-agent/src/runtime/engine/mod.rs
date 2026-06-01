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

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::approval::{block_on_approval_future, ApprovalRequest};
use crate::events::RunEvent;
use crate::llm::{LlmClient, LlmError, LlmRequest};
use crate::memory::{
    provider::block_on_memory_future, CompactionExhaustedError, MemoryManager, MemoryProvider,
};
use crate::tools::{ApprovalDecision, ToolContext, ToolSpecKind};
use crate::types::{
    AgentResult, AgentStatus, AgentTask, Message, ToolCall, ToolDirective, ToolExecutionResult,
    ToolResultStatus,
};

use super::cancellation::CancellationToken;
use super::context::ExecutionContext;

use super::cycle_runner::{is_prompt_too_long_error, MAX_PROMPT_TOO_LONG_RETRIES};
use super::results::assistant_message_from_response;
use super::token_usage::normalize_token_usage;
use super::tool_call_runner::{execute_tool_result, needs_tool_call_id, skipped_tool_result};

use self::completion::{
    handle_directive_result, handle_no_tool_response, DirectiveResultRequest, NoToolResponseRequest,
};
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
                let memory_compact_event = memory_compact_started_event(
                    controls.execution_context.as_ref(),
                    &memory_manager,
                    &task,
                    cycle_index,
                    &pre_compact_messages,
                    previous_prompt_tokens,
                    recent_tool_call_ids.as_ref(),
                );
                if let Some(event) = memory_compact_event.as_ref() {
                    notify_memory_before_compact(controls.execution_context.as_ref(), event);
                }
                let (prepared_messages, memory_compacted) = memory_manager
                    .compact_for_cycle_with_usage(
                        &pre_compact_messages,
                        cycle_index,
                        false,
                        previous_prompt_tokens,
                        recent_tool_call_ids.as_ref(),
                    );
                if memory_compacted {
                    if let Some(started_event) = memory_compact_event.as_ref() {
                        let completed = RunEvent::memory_compact_completed(
                            started_event.run_id().to_string(),
                            started_event.trace_id().to_string(),
                            started_event.agent_name().unwrap_or("agent").to_string(),
                            cycle_index,
                            pre_compact_messages.len(),
                            prepared_messages.len(),
                            None,
                        );
                        notify_memory_after_compact(
                            controls.execution_context.as_ref(),
                            &completed,
                        );
                    }
                }
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
                    if let Some(execution_context) = controls.execution_context.as_ref() {
                        request.metadata = serde_json::to_value(&execution_context.metadata)
                            .unwrap_or(Value::Null);
                    }
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
                if !cycle.token_usage.has_usage() {
                    cycle.token_usage =
                        normalize_token_usage(response.raw.get("usage").unwrap_or(&Value::Null));
                }
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
                    model_provider: controls.model_provider.clone(),
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
                    self.emit_log(
                        &controls,
                        "tool_call_started",
                        BTreeMap::from([
                            ("task_id".to_string(), Value::String(task.task_id.clone())),
                            (
                                "agent_name".to_string(),
                                Value::String(
                                    task.metadata
                                        .get("agent_name")
                                        .and_then(Value::as_str)
                                        .unwrap_or(&task.task_id)
                                        .to_string(),
                                ),
                            ),
                            ("cycle".to_string(), Value::from(cycle_index)),
                            (
                                "tool_name".to_string(),
                                Value::String(patched_call.name.clone()),
                            ),
                            (
                                "tool_arguments".to_string(),
                                Value::Object(patched_call.arguments.clone().into_iter().collect()),
                            ),
                            (
                                "tool_call_id".to_string(),
                                Value::String(patched_call.id.clone()),
                            ),
                        ]),
                    );
                    let tool_kind = self
                        .tool_registry
                        .get(&patched_call.name)
                        .map(|spec| spec.kind)
                        .ok();
                    if matches!(
                        tool_kind,
                        Some(ToolSpecKind::Agent | ToolSpecKind::BackgroundAgent)
                    ) {
                        self.emit_log(
                            &controls,
                            "sub_run_started",
                            BTreeMap::from([
                                ("task_id".to_string(), Value::String(task.task_id.clone())),
                                (
                                    "agent_name".to_string(),
                                    Value::String(
                                        task.metadata
                                            .get("agent_name")
                                            .and_then(Value::as_str)
                                            .unwrap_or(&task.task_id)
                                            .to_string(),
                                    ),
                                ),
                                ("cycle".to_string(), Value::from(cycle_index)),
                                (
                                    "parent_run_id".to_string(),
                                    Value::String(task.task_id.clone()),
                                ),
                                (
                                    "parent_tool_call_id".to_string(),
                                    Value::String(patched_call.id.clone()),
                                ),
                                (
                                    "task_id_hint".to_string(),
                                    Value::String(format!("sub_run:{}", patched_call.id)),
                                ),
                            ]),
                        );
                    }
                    let provider_approval_result = approval_provider_result(
                        self,
                        &controls,
                        &task,
                        cycle_index,
                        &patched_call,
                    );
                    let mut result = if let Some(mut result) = short_circuit_result {
                        if needs_tool_call_id(&result.tool_call_id) {
                            result.tool_call_id = call.id.clone();
                        }
                        result
                    } else if let Some(result) = provider_approval_result {
                        result
                    } else {
                        let mut result =
                            execute_tool_result(&self.tool_registry, &patched_call, &mut context);
                        if needs_tool_call_id(&result.tool_call_id) {
                            result.tool_call_id = patched_call.id.clone();
                        }
                        result
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
                    if matches!(
                        tool_kind,
                        Some(ToolSpecKind::Agent | ToolSpecKind::BackgroundAgent)
                    ) {
                        self.emit_log(
                            &controls,
                            "sub_run_completed",
                            BTreeMap::from([
                                ("task_id".to_string(), Value::String(task.task_id.clone())),
                                (
                                    "agent_name".to_string(),
                                    Value::String(
                                        task.metadata
                                            .get("agent_name")
                                            .and_then(Value::as_str)
                                            .unwrap_or(&task.task_id)
                                            .to_string(),
                                    ),
                                ),
                                ("cycle".to_string(), Value::from(cycle_index)),
                                (
                                    "parent_run_id".to_string(),
                                    Value::String(task.task_id.clone()),
                                ),
                                (
                                    "parent_tool_call_id".to_string(),
                                    Value::String(patched_call.id.clone()),
                                ),
                                (
                                    "status".to_string(),
                                    serde_json::to_value(result.status).unwrap_or(Value::Null),
                                ),
                                (
                                    "final_output".to_string(),
                                    Value::String(result.content.clone()),
                                ),
                            ]),
                        );
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
                    if let Some(result) = handle_directive_result(DirectiveResultRequest {
                        runtime: self,
                        controls: &controls,
                        task: &task,
                        cycle_index,
                        result: directive_result,
                        messages,
                        cycles,
                        shared_state,
                    }) {
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

fn approval_provider_result<C: LlmClient>(
    runtime: &AgentRuntime<C>,
    controls: &RuntimeRunControls,
    task: &AgentTask,
    cycle_index: u32,
    call: &ToolCall,
) -> Option<ToolExecutionResult> {
    if call.name == "task_finish" {
        return None;
    }
    let execution_context = controls.execution_context.as_ref()?;
    let provider = execution_context.approval_provider.as_ref()?;
    let broker = execution_context.approval_broker.as_ref()?;
    let agent_name = task
        .metadata
        .get("agent_name")
        .and_then(Value::as_str)
        .unwrap_or(&task.task_id)
        .to_string();
    let request = ApprovalRequest::for_tool_call(
        task.task_id.clone(),
        task.task_id.clone(),
        agent_name,
        cycle_index,
        call,
    );
    if !provider.should_request(&request) {
        return None;
    }

    let decision = match block_on_approval_future(provider.decide(&request)) {
        Ok(Some(ApprovalDecision::Approved)) => return None,
        Ok(Some(ApprovalDecision::Denied(reason))) => ApprovalDecision::Denied(reason),
        Ok(Some(ApprovalDecision::TimedOut(reason))) => ApprovalDecision::TimedOut(reason),
        Ok(Some(ApprovalDecision::NeedsApproval)) | Ok(None) => {
            if let Err(error) = broker.register(request.clone()) {
                return Some(approval_error_result(
                    call,
                    "approval_broker_error",
                    error.to_string(),
                ));
            }
            runtime.emit_log(
                controls,
                "approval_requested",
                BTreeMap::from([
                    ("task_id".to_string(), Value::String(request.run_id.clone())),
                    (
                        "agent_name".to_string(),
                        Value::String(request.agent_name.clone()),
                    ),
                    ("cycle".to_string(), Value::from(cycle_index)),
                    (
                        "request_id".to_string(),
                        Value::String(request.request_id.clone()),
                    ),
                    (
                        "tool_call_id".to_string(),
                        Value::String(request.tool_call_id.clone()),
                    ),
                    (
                        "tool_name".to_string(),
                        Value::String(request.tool_name.clone()),
                    ),
                    (
                        "preview".to_string(),
                        Value::String(request.preview.clone()),
                    ),
                ]),
            );
            broker
                .wait_blocking(&request.request_id, execution_context.approval_timeout)
                .unwrap_or_else(|error| ApprovalDecision::deny(error.to_string()))
        }
        Err(error) => ApprovalDecision::deny(error.to_string()),
    };

    runtime.emit_log(
        controls,
        "approval_resolved",
        BTreeMap::from([
            ("task_id".to_string(), Value::String(request.run_id.clone())),
            (
                "agent_name".to_string(),
                Value::String(request.agent_name.clone()),
            ),
            ("cycle".to_string(), Value::from(cycle_index)),
            (
                "request_id".to_string(),
                Value::String(request.request_id.clone()),
            ),
            (
                "tool_call_id".to_string(),
                Value::String(request.tool_call_id.clone()),
            ),
            (
                "tool_name".to_string(),
                Value::String(request.tool_name.clone()),
            ),
            (
                "approved".to_string(),
                Value::Bool(matches!(decision, ApprovalDecision::Approved)),
            ),
        ]),
    );

    match decision {
        ApprovalDecision::Approved => None,
        ApprovalDecision::NeedsApproval => Some(approval_error_result(
            call,
            "approval_unresolved",
            "Approval was not resolved.",
        )),
        ApprovalDecision::Denied(reason) => {
            Some(approval_error_result(call, "approval_denied", reason))
        }
        ApprovalDecision::TimedOut(reason) => {
            Some(approval_error_result(call, "approval_timeout", reason))
        }
    }
}

fn memory_compact_started_event(
    execution_context: Option<&ExecutionContext>,
    memory_manager: &MemoryManager,
    task: &AgentTask,
    cycle_index: u32,
    messages: &[Message],
    previous_prompt_tokens: Option<u64>,
    recent_tool_call_ids: Option<&BTreeSet<String>>,
) -> Option<RunEvent> {
    let providers = memory_providers(execution_context);
    if providers.is_empty() {
        return None;
    }
    let usage = memory_manager.estimate_memory_usage_percentage(
        messages,
        previous_prompt_tokens,
        recent_tool_call_ids,
    );
    if usage <= 100 {
        return None;
    }
    let agent_name = task
        .metadata
        .get("agent_name")
        .and_then(Value::as_str)
        .unwrap_or(&task.task_id)
        .to_string();
    Some(RunEvent::memory_compact_started(
        task.task_id.clone(),
        task.task_id.clone(),
        agent_name,
        cycle_index,
        messages.len(),
        previous_prompt_tokens,
    ))
}

fn notify_memory_before_compact(execution_context: Option<&ExecutionContext>, event: &RunEvent) {
    for provider in memory_providers(execution_context) {
        let _ = block_on_memory_future(provider.before_compact(event));
    }
}

fn notify_memory_after_compact(execution_context: Option<&ExecutionContext>, event: &RunEvent) {
    for provider in memory_providers(execution_context) {
        let _ = block_on_memory_future(provider.after_compact(event));
    }
}

fn memory_providers(
    execution_context: Option<&ExecutionContext>,
) -> Vec<&std::sync::Arc<dyn MemoryProvider>> {
    execution_context
        .map(|context| context.memory_providers.iter().collect())
        .unwrap_or_default()
}

fn approval_error_result(
    call: &ToolCall,
    error_code: &str,
    message: impl Into<String>,
) -> ToolExecutionResult {
    let message = message.into();
    ToolExecutionResult {
        tool_call_id: call.id.clone(),
        content: serde_json::json!({
            "ok": false,
            "error": message,
            "error_code": error_code,
            "tool_name": call.name.clone(),
        })
        .to_string(),
        status: ToolResultStatus::Error,
        directive: ToolDirective::Continue,
        error_code: Some(error_code.to_string()),
        metadata: BTreeMap::new(),
        image_url: None,
        image_path: None,
    }
}
