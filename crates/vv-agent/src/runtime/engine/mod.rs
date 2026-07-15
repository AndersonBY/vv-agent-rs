mod approval;
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
use std::sync::Arc;

use futures_util::FutureExt;
use serde_json::Value;

use crate::llm::{LlmClient, LlmError, LlmRequest, LlmStreamCallback};
use crate::memory::CompactionExhaustedError;
use crate::tools::{ToolContext, ToolError, ToolOrchestrator, ToolRunOptions, ToolSpecKind};
use crate::types::{
    last_assistant_output, AgentResult, AgentStatus, AgentTask, CompletionReason, ToolDirective,
    ToolExecutionResult,
};

use super::cancellation::CancellationToken;

use super::cycle_runner::{is_prompt_too_long_error, MAX_PROMPT_TOO_LONG_RETRIES};
use super::results::assistant_message_from_response;
use super::token_usage::normalize_token_usage;
use super::tool_call_runner::{apply_tool_use_behavior, needs_tool_call_id, skipped_tool_result};

use self::approval::{approval_error_result, approval_provider_result, PendingToolApprovalCapture};
use self::completion::{
    handle_directive_result, handle_no_tool_response, DirectiveResultRequest, NoToolResponseRequest,
};
use self::helpers::{
    cancelled_agent_result, collect_interruption_messages, controls_cancelled,
    drain_steering_queue, failed_agent_result, image_notification_from_tool_result,
    previous_cycle_memory_usage,
};
use self::memory::{
    memory_compact_completed_event, memory_compact_event_payload, memory_compact_started_event,
    notify_memory_after_compact, notify_memory_before_compact,
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
    pub fn set_tool_policy(&mut self, tool_policy: crate::tools::ToolPolicy) {
        self.tool_policy = Some(tool_policy);
    }

    pub fn run(&self, task: AgentTask) -> Result<AgentResult, LlmError> {
        self.run_with_controls(task, RuntimeRunControls::default())
    }

    pub fn run_with_controls(
        &self,
        task: AgentTask,
        mut controls: RuntimeRunControls,
    ) -> Result<AgentResult, LlmError> {
        if let Some(context) = controls.execution_context.as_mut() {
            if context.approval_provider.is_some() && context.approval_broker.is_none() {
                context.approval_broker = Some(crate::approval::ApprovalBroker::default());
            }
        }
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
        self.emit_log(
            &controls,
            "agent_started",
            BTreeMap::from([("model".to_string(), Value::String(task.model.clone()))]),
        );

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
        let cycle_index_start = controls.cycle_index_start.unwrap_or(1);
        let cycle_count = controls.cycle_count.unwrap_or(task.max_cycles);
        let mut result = self.execution_backend.execute_with_state(
            &task,
            messages,
            cycles,
            shared_state,
            |cycle_index, messages, cycles, shared_state, cancellation_token| {
                let _cancellation_scope = CancellationToken::enter_scope(cancellation_token);
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
                    false,
                )
                .map(|event| {
                    let event = notify_memory_before_compact(
                        controls.execution_context.as_ref(),
                        event,
                        &pre_compact_messages,
                    );
                    self.emit_log(
                        &controls,
                        "memory_compact_started",
                        memory_compact_event_payload(&event),
                    );
                    event
                });
                let (mut compacted_messages, memory_compacted) = memory_manager
                    .compact_for_cycle_with_usage(
                        &pre_compact_messages,
                        cycle_index,
                        false,
                        previous_prompt_tokens,
                        recent_tool_call_ids.as_ref(),
                    );
                if let Some(started_event) = memory_compact_event.as_ref() {
                    let completed = memory_compact_completed_event(
                        started_event,
                        cycle_index,
                        &pre_compact_messages,
                        &compacted_messages,
                        &memory_manager.config.model,
                    );
                    let completed =
                        notify_memory_after_compact(controls.execution_context.as_ref(), completed);
                    self.emit_log(
                        &controls,
                        "memory_compact_completed",
                        memory_compact_event_payload(&completed),
                    );
                }
                *messages = compacted_messages.clone();
                let tool_schemas = self.planned_tool_schemas(&task);
                let llm_messages = memory_manager.apply_session_memory_context(&compacted_messages);
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
                self.emit_log(
                    &controls,
                    "llm_started",
                    BTreeMap::from([
                        ("cycle".to_string(), Value::from(cycle_index)),
                        ("model".to_string(), Value::String(task.model.clone())),
                        (
                            "message_count".to_string(),
                            Value::from(request_messages.len()),
                        ),
                    ]),
                );
                let cycle_stream_callback = effective_stream_callback.as_ref().map(|callback| {
                    let callback = callback.clone();
                    Arc::new(move |event: &BTreeMap<String, Value>| {
                        let mut event = event.clone();
                        event.insert("cycle".to_string(), Value::from(cycle_index));
                        callback(&event);
                    }) as LlmStreamCallback
                });
                let response = loop {
                    let mut request = LlmRequest::new(task.model.clone(), request_messages.clone());
                    request.tools = request_tool_schemas.clone();
                    let mut request_metadata = task.metadata.clone();
                    if let Some(execution_context) = controls.execution_context.as_ref() {
                        request_metadata.extend(execution_context.metadata.clone());
                    }
                    request.metadata = Value::Object(request_metadata.into_iter().collect());
                    request.model_settings = task.model_settings.clone();
                    match self
                        .llm_client
                        .complete_with_stream(request, cycle_stream_callback.clone())
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
                            let before_retry_compact = compacted_messages.clone();
                            let started = memory_compact_started_event(
                                controls.execution_context.as_ref(),
                                &memory_manager,
                                &task,
                                cycle_index,
                                &before_retry_compact,
                                None,
                                recent_tool_call_ids.as_ref(),
                                true,
                            )
                            .expect("forced memory compaction always starts a lifecycle");
                            let started = notify_memory_before_compact(
                                controls.execution_context.as_ref(),
                                started,
                                &before_retry_compact,
                            );
                            self.emit_log(
                                &controls,
                                "memory_compact_started",
                                memory_compact_event_payload(&started),
                            );
                            compacted_messages = if prompt_too_long_retries == 1 {
                                let (compacted, _) = memory_manager.compact_for_cycle(
                                    &compacted_messages,
                                    cycle_index,
                                    true,
                                );
                                compacted
                            } else {
                                memory_manager.emergency_compact(
                                    &compacted_messages,
                                    (0.2 * prompt_too_long_retries as f64).min(0.95),
                                )
                            };
                            let completed = memory_compact_completed_event(
                                &started,
                                cycle_index,
                                &before_retry_compact,
                                &compacted_messages,
                                &memory_manager.config.model,
                            );
                            let completed = notify_memory_after_compact(
                                controls.execution_context.as_ref(),
                                completed,
                            );
                            self.emit_log(
                                &controls,
                                "memory_compact_completed",
                                memory_compact_event_payload(&completed),
                            );
                            let retry_tool_schemas = self.planned_tool_schemas(&task);
                            let llm_messages =
                                memory_manager.apply_session_memory_context(&compacted_messages);
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
                    super::sub_agents::SubTaskRunControls {
                        parent_cancellation_token: cancellation_token.cloned(),
                        stream_callback: effective_stream_callback.clone(),
                        parent_log_handler: self.log_handler.clone(),
                        parent_event_handler: controls.log_handler.clone(),
                        parent_execution_context: controls.execution_context.clone(),
                        model_provider: controls.model_provider.clone(),
                        parent_run_context: controls.run_context.clone(),
                        tool_policy: self.tool_policy.clone(),
                    },
                );
                let mut tool_metadata = task.metadata.clone();
                for key in [
                    "_vv_agent_agent_name",
                    "_vv_agent_parent_run_id",
                    "_vv_agent_parent_tool_call_id",
                    "_vv_agent_run_id",
                    "_vv_agent_session_id",
                    "_vv_agent_trace_id",
                ] {
                    tool_metadata.remove(key);
                }
                if let Some(execution_context) = controls.execution_context.as_ref() {
                    tool_metadata.extend(execution_context.metadata.clone());
                }
                let trace_id = controls
                    .execution_context
                    .as_ref()
                    .and_then(|context| {
                        ["_vv_agent_trace_id", "trace_id"]
                            .into_iter()
                            .find_map(|key| {
                                context
                                    .metadata
                                    .get(key)
                                    .and_then(Value::as_str)
                                    .map(str::trim)
                                    .filter(|value| !value.is_empty())
                                    .map(str::to_string)
                            })
                    })
                    .or_else(|| {
                        controls.run_context.as_ref().and_then(|run| {
                            ["_vv_agent_trace_id", "trace_id"]
                                .into_iter()
                                .find_map(|key| {
                                    run.metadata
                                        .get(key)
                                        .and_then(Value::as_str)
                                        .map(str::trim)
                                        .filter(|value| !value.is_empty())
                                        .map(str::to_string)
                                })
                        })
                    })
                    .or_else(|| {
                        ["_vv_agent_trace_id", "trace_id"]
                            .into_iter()
                            .find_map(|key| {
                                task.metadata
                                    .get(key)
                                    .and_then(Value::as_str)
                                    .map(str::trim)
                                    .filter(|value| !value.is_empty())
                                    .map(str::to_string)
                            })
                    });
                let parent_run_id = controls
                    .run_context
                    .as_ref()
                    .map(|run| run.run_id.trim())
                    .filter(|run_id| !run_id.is_empty())
                    .map(str::to_string)
                    .or_else(|| {
                        controls.execution_context.as_ref().and_then(|context| {
                            context
                                .metadata
                                .get("_vv_agent_run_id")
                                .and_then(Value::as_str)
                                .map(str::trim)
                                .filter(|value| !value.is_empty())
                                .map(str::to_string)
                        })
                    });
                let mut context = ToolContext {
                    workspace: workspace_path.clone(),
                    shared_state: shared_state.clone(),
                    cycle_index,
                    task_id: task.task_id.clone(),
                    tool_call_id: String::new(),
                    tool_name: String::new(),
                    arguments: crate::types::ToolArguments::new(),
                    metadata: tool_metadata,
                    app_state: controls
                        .execution_context
                        .as_ref()
                        .and_then(|context| context.app_state.clone()),
                    workspace_backend: workspace_backend.clone(),
                    model_provider: controls.model_provider.clone(),
                    run_context: controls.run_context.clone(),
                    sub_task_runner,
                    sub_task_manager: Some(sub_task_manager.clone()),
                    sub_task_turn_snapshot: Some(super::sub_task_manager::SubTaskTurnSnapshot {
                        cancellation_token: cancellation_token.cloned(),
                        event_handler: controls.log_handler.clone(),
                        stream_callback: effective_stream_callback.clone(),
                        trace_id,
                        parent_run_id,
                        parent_tool_call_id: None,
                        parent_execution_context: controls.execution_context.clone(),
                        parent_run_context: controls.run_context.clone(),
                        tool_policy: self.tool_policy.clone().unwrap_or_default(),
                    }),
                    execution_backend: Some(self.execution_backend.clone()),
                    background_parent_run_config: controls.background_parent_run_config.clone(),
                };

                let mut directive_result = None;
                let mut directive_completion_reason = None;
                let mut directive_completion_tool_name = None;
                let mut image_notifications = Vec::new();
                let tool_orchestrator =
                    ToolOrchestrator::from_tools(self.tool_registry.executors());
                let planned_tool_names = request_tool_schemas
                    .iter()
                    .filter_map(|schema| schema["function"]["name"].as_str().map(str::to_string))
                    .collect::<Vec<_>>();
                let tool_run_options = self
                    .tool_policy
                    .as_ref()
                    .map(ToolRunOptions::from_policy)
                    .unwrap_or_default()
                    .planned_names(planned_tool_names);
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
                    let mut result = if let Some(mut result) = short_circuit_result {
                        if needs_tool_call_id(&result.tool_call_id) {
                            result.tool_call_id = call.id.clone();
                        }
                        result
                    } else {
                        let mut approval_failure = None;
                        let mut result = match block_on_engine_tool_run(
                            tool_orchestrator.run_one_with_approval_and_metadata(
                                patched_call.clone(),
                                &mut context,
                                tool_run_options.clone(),
                                |call, effective_requirement, approval_context, tool_metadata| {
                                    let result = match approval_provider_result(
                                        self,
                                        &controls,
                                        &task,
                                        cycle_index,
                                        call,
                                        effective_requirement,
                                        tool_metadata,
                                    ) {
                                        Ok(result) => result,
                                        Err(error) => {
                                            approval_failure = Some(error);
                                            return Some(approval_error_result(
                                                call,
                                                "approval_provider_error",
                                                "Approval provider failed.",
                                            ));
                                        }
                                    };
                                    if result.as_ref().is_some_and(|result| {
                                        result.error_code.as_deref()
                                            == Some("tool_approval_required")
                                    }) {
                                        self.capture_pending_tool_approval(
                                            PendingToolApprovalCapture {
                                                task: &task,
                                                hook_manager: &hook_manager,
                                                cycle_index,
                                                call,
                                                context: approval_context,
                                                options: &tool_run_options,
                                                orchestrator: &tool_orchestrator,
                                                result: result
                                                    .as_ref()
                                                    .expect("checked approval result"),
                                            },
                                        );
                                    }
                                    result
                                },
                            ),
                        ) {
                            Ok(result) => result,
                            Err(error) => approval_error_result(
                                &patched_call,
                                "tool_orchestrator_error",
                                error.to_string(),
                            ),
                        };
                        if let Some(error) = approval_failure {
                            *shared_state = context.shared_state.clone();
                            cycles.push(cycle);
                            self.emit_log(
                                &controls,
                                "cycle_failed",
                                BTreeMap::from([
                                    ("cycle".to_string(), Value::from(cycle_index)),
                                    ("error".to_string(), Value::String(error.to_string())),
                                ]),
                            );
                            return Some(failed_agent_result(
                                messages.clone(),
                                cycles.clone(),
                                shared_state.clone(),
                                error.to_string(),
                            ));
                        }
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
                    let behavior_reason =
                        apply_tool_use_behavior(&task, &patched_call, &mut result);
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
                                    self::logging::tool_result_status_value(result.status),
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
                        directive_completion_reason =
                            behavior_reason.or(Some(match result.directive {
                                ToolDirective::WaitUser => CompletionReason::WaitUser,
                                ToolDirective::Finish => CompletionReason::ToolFinish,
                                ToolDirective::Continue => unreachable!(),
                            }));
                        directive_completion_tool_name = Some(patched_call.name.clone());
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
                        completion_reason: directive_completion_reason
                            .expect("terminal tool directive has a completion reason"),
                        completion_tool_name: directive_completion_tool_name
                            .as_deref()
                            .expect("terminal tool directive has a tool name"),
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
            cycle_index_start,
            cycle_count,
        );
        if let Some(error) = pending_error {
            return Err(error);
        }
        if result.status == AgentStatus::Failed
            && effective_cancellation_token
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
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
            self.emit_run_max_cycles(&controls, &result);
        }
        Ok(result)
    }
}

fn block_on_engine_tool_run<'a>(
    future: impl std::future::Future<Output = Result<ToolExecutionResult, ToolError>> + 'a,
) -> Result<ToolExecutionResult, ToolError> {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread {
            tokio::task::block_in_place(|| handle.block_on(future))
        } else {
            future.now_or_never().unwrap_or_else(|| {
                Err(ToolError::new(
                    "tool future cannot be driven from a current-thread runtime",
                ))
            })
        }
    } else {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| ToolError::new(error.to_string()))?
            .block_on(future)
    }
}
