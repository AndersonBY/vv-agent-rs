mod approval;
mod budget;
mod checkpoint;
mod completion;
mod construction;
mod controls;
mod cycle_inputs;
mod helpers;
mod lifecycle;
mod logging;
mod memory;
mod model_request;
mod planning;
mod run_setup;
mod session_api;
mod state;
mod tool_batch;

use serde_json::Value;
use std::collections::BTreeMap;

use crate::llm::{LlmClient, LlmError};
use crate::memory::CompactionExhaustedError;
use crate::tools::ToolSpecKind;
use crate::types::{AgentResult, AgentTask, CompletionReason, ToolDirective, ToolExecutionResult};

use super::cancellation::CancellationToken;

use super::cycle_runner::{is_prompt_too_long_error, MAX_PROMPT_TOO_LONG_RETRIES};
use super::model_calls::ModelCallDispatchRequest;
use super::results::assistant_message_from_response;
use super::tool_call_runner::{apply_tool_use_behavior, needs_tool_call_id, skipped_tool_result};

use self::approval::{approval_error_result, approval_provider_result, PendingToolApprovalCapture};
use self::budget::{
    budget_snapshot, enforce_cycle_start, finalize_run_budget, lock_budget,
    observe_tool_batch_completion, preflight_tool_batch, project_model_call_completion,
    PreparedRunBudget,
};
use self::checkpoint::{CheckpointModelCompletion, CheckpointToolPlan};
use self::helpers::{
    cancelled_agent_result, collect_interruption_messages, controls_cancelled,
    drain_steering_queue, failed_agent_result, finalize_terminal_projection,
    image_notification_from_tool_result, project_cycle_cancellation, task_token_usage,
};
use self::lifecycle::{
    finalize_no_tool_cycle, finalize_tool_cycle, NoToolCycleFinalization, ToolCycleFinalization,
};
use self::memory::{
    compact_cycle_memory, memory_compact_completed_event, memory_compact_event_payload,
    memory_compact_started_event, memory_inference_failure_result, notify_memory_after_compact,
    notify_memory_before_compact, CycleMemoryCompaction,
};
use self::model_request::{
    build_model_request, cycle_stream_callback, effective_model_call_target,
};
use self::planning::block_on_engine_tool_run;
use self::run_setup::{
    prepare_approval_broker, prepare_run_setup, prepare_runtime_accounting, PreparedRun,
    PreparedRuntimeAccounting,
};
pub use self::state::AgentRuntime;
use self::tool_batch::{PreparedToolBatch, ToolBatchSetup};

pub use controls::{
    BeforeCycleMessageProvider, CheckpointRuntimeControl, InterruptionMessageProvider,
    RunEventHandler, RuntimeRunControls,
};
pub(crate) use helpers::build_initial_messages;
pub use session_api::*;

impl<C: LlmClient + Clone + 'static> AgentRuntime<C> {
    pub fn run_with_controls(
        &self,
        mut task: AgentTask,
        mut controls: RuntimeRunControls,
    ) -> Result<AgentResult, LlmError> {
        if let Some(policy) = self.tool_policy.as_ref() {
            crate::runtime::tool_planner::project_tool_policy(&mut task, policy);
        }
        prepare_approval_broker(&mut controls);
        let PreparedRun {
            task,
            messages,
            cycles,
            shared_state,
            workspace_path,
            workspace_backend,
            sub_task_manager,
            memory_manager,
            memory_model_provider,
        } = prepare_run_setup(self, task, &controls)?;
        let cycle_index_start = controls.cycle_index_start.unwrap_or(1);
        let backend_manages_checkpoint_cycles = self.execution_backend.manages_checkpoint_cycles();
        self.emit_run_started(&controls, &task, &workspace_path);
        let PreparedRunBudget {
            limits: effective_budget_limits,
            controller: budget_controller,
            early_result,
        } = self.prepare_run_budget(&controls, &messages, &cycles, &shared_state);
        let configured_budget = effective_budget_limits.is_some();
        let child_budget_limits = effective_budget_limits.clone();

        let effective_cancellation_token = controls.effective_cancellation_token();
        let PreparedRuntimeAccounting {
            model_call_ledger,
            model_call_coordinator,
            checkpoint,
            mut memory_manager,
        } = prepare_runtime_accounting(
            self,
            &task,
            &mut controls,
            memory_manager,
            memory_model_provider,
            &budget_controller,
        )?;
        if !backend_manages_checkpoint_cycles {
            if let Some(result) = checkpoint.begin_run_cycle(cycle_index_start)? {
                return Ok(result);
            }
        }
        if let Some(mut result) = early_result {
            result.token_usage = model_call_ledger.usage();
            return Ok(result);
        }
        self.emit_log(
            &controls,
            "agent_started",
            BTreeMap::from([("model".to_string(), Value::String(task.model.clone()))]),
        );
        if !configured_budget && controls_cancelled(&controls) {
            self.emit_log(
                &controls,
                "run_cancelled",
                BTreeMap::from([(
                    "error".to_string(),
                    Value::String("Operation was cancelled".to_string()),
                )]),
            );
            return Ok(cancelled_agent_result(
                messages,
                cycles,
                shared_state,
                task_token_usage(&controls),
            ));
        }
        let effective_event_handler = controls.effective_event_handler();
        let mut pending_error = None;
        let cycle_count = controls.cycle_count.unwrap_or(task.max_cycles);
        let mut result = self.execution_backend.execute_with_state(
            &task,
            messages,
            cycles,
            shared_state,
            |cycle_index, messages, cycles, shared_state, cancellation_token| {
                let _cancellation_scope = CancellationToken::enter_scope(cancellation_token);
                if !backend_manages_checkpoint_cycles {
                    if let Some(result) =
                        checkpoint.begin_cycle(cycle_index, messages, cycles, shared_state)
                    {
                        return Some(result);
                    }
                }
                if let Some(result) = project_cycle_cancellation(
                    self,
                    &controls,
                    cycle_index,
                    cancellation_token,
                    messages,
                    cycles,
                    shared_state,
                ) {
                    return Some(result);
                }
                let active_after_cycle_denials = match self.read_after_cycle_denials(
                    &controls,
                    cycle_index,
                    messages,
                    cycles,
                    shared_state,
                ) {
                    Ok(denials) => denials,
                    Err(result) => return Some(*result),
                };
                self.apply_cycle_inputs(&controls, cycle_index, messages, shared_state);
                if let Some(result) = project_cycle_cancellation(
                    self,
                    &controls,
                    cycle_index,
                    cancellation_token,
                    messages,
                    cycles,
                    shared_state,
                ) {
                    return Some(result);
                }
                if let Some(result) = enforce_cycle_start(
                    &budget_controller,
                    &controls,
                    cycle_index,
                    messages,
                    cycles,
                    shared_state,
                ) {
                    return Some(result);
                }
                if let Some(result) = checkpoint.update_budget_usage(
                    || budget_snapshot(&budget_controller),
                    messages,
                    cycles,
                    shared_state,
                ) {
                    return Some(result);
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
                let CycleMemoryCompaction {
                    messages: mut compacted_messages,
                    changed: memory_compacted,
                    recent_tool_call_ids,
                } = match compact_cycle_memory(
                    self,
                    &controls,
                    &task,
                    &hook_manager,
                    &mut memory_manager,
                    cycle_index,
                    messages,
                    cycles,
                    shared_state,
                    &model_call_ledger,
                ) {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        return Some(memory_inference_failure_result(
                            error,
                            &checkpoint,
                            &budget_controller,
                            &controls,
                            messages,
                            cycles,
                            shared_state,
                        ));
                    }
                };
                *messages = compacted_messages.clone();
                let tool_schemas = self.planned_tool_schemas_with_after_cycle_denials(
                    &task,
                    &active_after_cycle_denials,
                );
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
                let mut stream_metadata = task.metadata.clone();
                if let Some(execution_context) = controls.execution_context.as_ref() {
                    stream_metadata.extend(execution_context.metadata.clone());
                }
                let cycle_stream_callback = cycle_stream_callback(
                    effective_event_handler.as_ref(),
                    &stream_metadata,
                    cycle_index,
                );
                let response = loop {
                    let request = build_model_request(
                        &task,
                        &controls,
                        &request_messages,
                        &request_tool_schemas,
                    );
                    let (effective_backend, effective_model) = effective_model_call_target(
                        &task,
                        &controls,
                        self.default_backend.as_deref(),
                    );
                    let operation_slot = if prompt_too_long_retries == 0 {
                        "main".to_string()
                    } else {
                        format!("prompt_too_long_{prompt_too_long_retries}")
                    };
                    let completion = checkpoint.complete_model(
                        ModelCallDispatchRequest {
                            cycle_index,
                            operation_slot: &operation_slot,
                            operation: crate::types::ModelCallOperation::AgentCycle,
                            backend: &effective_backend,
                            model: &effective_model,
                            request: &request,
                            accounting: &model_call_coordinator,
                        },
                        || budget_snapshot(&budget_controller),
                        |request| {
                            self.llm_client
                                .complete_with_stream(request, cycle_stream_callback.clone())
                        },
                        (messages, cycles, shared_state),
                    );
                    let completion = match completion {
                        CheckpointModelCompletion::Continue(completion) => *completion,
                        CheckpointModelCompletion::Stop(result) => return Some(*result),
                    };
                    match completion {
                        Ok(dispatch) => break dispatch,
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
                                    task_token_usage(&controls),
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
                            let compaction_mode;
                            compacted_messages = if prompt_too_long_retries == 1 {
                                let outcome = match memory_manager
                                    .compact_for_cycle_with_usage_observed(
                                        &compacted_messages,
                                        cycle_index,
                                        true,
                                        None,
                                        recent_tool_call_ids.as_ref(),
                                    ) {
                                    Ok(outcome) => outcome,
                                    Err(error) => {
                                        return Some(memory_inference_failure_result(
                                            error,
                                            &checkpoint,
                                            &budget_controller,
                                            &controls,
                                            messages,
                                            cycles,
                                            shared_state,
                                        ));
                                    }
                                };
                                compaction_mode = outcome.mode;
                                outcome.messages
                            } else {
                                let emergency = memory_manager.emergency_compact(
                                    &compacted_messages,
                                    (0.2 * prompt_too_long_retries as f64).min(0.95),
                                );
                                compaction_mode = if emergency == compacted_messages {
                                    crate::events::MemoryCompactMode::None
                                } else {
                                    crate::events::MemoryCompactMode::Emergency
                                };
                                emergency
                            };
                            let completed = memory_compact_completed_event(
                                &started,
                                cycle_index,
                                &before_retry_compact,
                                &compacted_messages,
                                &memory_manager.config.model,
                                compaction_mode,
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
                            let retry_tool_schemas = self
                                .planned_tool_schemas_with_after_cycle_denials(
                                    &task,
                                    &active_after_cycle_denials,
                                );
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
                                task_token_usage(&controls),
                            ));
                        }
                    }
                };
                let model_budget_exhaustion = response.budget_exhaustion.clone();
                let model_usage = response.usage.clone();
                let response = hook_manager.apply_after_llm(
                    &task,
                    cycle_index,
                    &request_messages,
                    &request_tool_schemas,
                    response.response,
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

                let model_boundary_result = project_model_call_completion(
                    &budget_controller,
                    &controls,
                    model_budget_exhaustion,
                    cancellation_token,
                    &cycle,
                    messages,
                    cycles,
                    shared_state,
                );
                if let Some(result) = checkpoint.update_budget_usage(
                    || budget_snapshot(&budget_controller),
                    messages,
                    cycles,
                    shared_state,
                ) {
                    return Some(result);
                }
                if let Some(result) = model_boundary_result {
                    return Some(result);
                }
                self.emit_cycle_llm_response(&controls, &cycle, &model_usage);

                if response.tool_calls.is_empty() {
                    return finalize_no_tool_cycle(NoToolCycleFinalization {
                        runtime: self,
                        controls: &controls,
                        task: &task,
                        cycle_index,
                        response: &response,
                        cycle,
                        messages,
                        cycles,
                        shared_state,
                        checkpoint: &checkpoint,
                        budget_controller: &budget_controller,
                        persisted_denials: &active_after_cycle_denials,
                    });
                }

                if let Some(result) = preflight_tool_batch(
                    &budget_controller,
                    &controls,
                    cycle_index,
                    &response.tool_calls,
                    &cycle,
                    messages,
                    cycles,
                    shared_state,
                ) {
                    return Some(result);
                }

                let PreparedToolBatch {
                    mut context,
                    orchestrator: tool_orchestrator,
                    options: tool_run_options,
                } = self.prepare_tool_batch(ToolBatchSetup {
                    task: &task,
                    controls: &controls,
                    workspace_path: &workspace_path,
                    workspace_backend: &workspace_backend,
                    shared_state,
                    sub_task_manager: &sub_task_manager,
                    cycle_index,
                    cancellation_token,
                    child_budget_limits: &child_budget_limits,
                    request_tool_schemas: &request_tool_schemas,
                    after_cycle_disallowed_tools: &active_after_cycle_denials,
                });

                let mut directive_result = None;
                let mut directive_completion_reason = None;
                let mut directive_completion_tool_name = None;
                let mut image_notifications = Vec::new();
                for (call_index, call) in response.tool_calls.iter().enumerate() {
                    if cancellation_token.is_some_and(CancellationToken::is_cancelled)
                        || controls_cancelled(&controls)
                    {
                        *shared_state = context.shared_state.clone();
                        if let Some(controller) = &budget_controller {
                            lock_budget(controller).tool_batch_complete(
                                &controls,
                                cycle_index,
                                false,
                                true,
                            );
                        }
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
                            task_token_usage(&controls),
                        ));
                    }
                    let (patched_call, short_circuit_result) = hook_manager.apply_before_tool_call(
                        &task,
                        cycle_index,
                        call.clone(),
                        &context,
                    );
                    let checkpoint_plan = checkpoint.plan_tool(
                        cycle_index,
                        &patched_call,
                        || {
                            let idempotency = super::run_definition::tool_idempotency_for(
                                &self.tool_registry,
                                &patched_call.name,
                            );
                            let budget_usage = budget_snapshot(&budget_controller);
                            (idempotency, budget_usage)
                        },
                        messages,
                        cycles,
                        shared_state,
                    );
                    let checkpoint_plan = match checkpoint_plan {
                        CheckpointToolPlan::Continue(plan) => plan,
                        CheckpointToolPlan::Stop(result) => return Some(*result),
                    };
                    let tool_kind = self
                        .tool_registry
                        .get(&patched_call.name)
                        .map(|spec| spec.kind)
                        .ok();
                    let mut approval_failure = None;
                    let mut execution = if let Some(mut result) = short_circuit_result {
                        if needs_tool_call_id(&result.tool_call_id) {
                            result.tool_call_id = call.id.clone();
                        }
                        tool_orchestrator.observe_result_without_execution(
                            patched_call.clone(),
                            result,
                            &tool_run_options,
                        )
                    } else if let Some(result) = checkpoint_plan
                        .as_ref()
                        .and_then(|plan| plan.replay_result.clone())
                    {
                        context.idempotency_key = checkpoint_plan
                            .as_ref()
                            .map(|plan| plan.idempotency_key.clone());
                        tool_orchestrator.observe_result_without_execution(
                            patched_call.clone(),
                            result,
                            &tool_run_options,
                        )
                    } else {
                        let effective_tool_run_options = checkpoint.before_tool_dispatch(
                            tool_run_options.clone().idempotency_key(
                                checkpoint_plan
                                    .as_ref()
                                    .map(|plan| plan.idempotency_key.clone()),
                            ),
                            cycle_index,
                        );
                        let execution = match block_on_engine_tool_run(
                            tool_orchestrator.run_one_with_approval_and_metadata_deferred(
                                patched_call.clone(),
                                &mut context,
                                effective_tool_run_options.clone(),
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
                                                options: &effective_tool_run_options,
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
                            Ok(execution) => execution,
                            Err(error) => crate::tools::orchestrator::DeferredToolExecution::without_lifecycle(
                                approval_error_result(
                                    &patched_call,
                                    "tool_orchestrator_error",
                                    error.to_string(),
                                ),
                            ),
                        };
                        if let Some(result) =
                            checkpoint.pending_failure(messages, cycles, shared_state)
                        {
                            return Some(result);
                        }
                        execution
                    };
                    let execution_started = execution.execution_started();
                    let mut result = execution.result().clone();
                    if needs_tool_call_id(&result.tool_call_id) {
                        result.tool_call_id = patched_call.id.clone();
                    }
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
                    execution.replace_result(result);
                    let result = execution.complete();
                    if let Some(error) = approval_failure {
                        *shared_state = context.shared_state.clone();
                        if let Some(controller) = &budget_controller {
                            lock_budget(controller).tool_batch_complete(
                                &controls,
                                cycle_index,
                                true,
                                false,
                            );
                        }
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
                            task_token_usage(&controls),
                        ));
                    }
                    if let Some(result) = checkpoint.finish_tool(
                        cycle_index,
                        &patched_call,
                        &result,
                        || budget_snapshot(&budget_controller),
                        (messages, cycles, shared_state),
                    ) {
                        return Some(result);
                    }
                    if matches!(
                        tool_kind,
                        Some(ToolSpecKind::Agent | ToolSpecKind::BackgroundAgent)
                    ) && execution_started
                    {
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
                            self.emit_skipped_tool_result(
                                &controls,
                                cycle_index,
                                skipped_call,
                                &skipped,
                            );
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
                            self.emit_skipped_tool_result(
                                &controls,
                                cycle_index,
                                skipped_call,
                                &skipped,
                            );
                            messages.push(skipped.to_message());
                            cycle.tool_results.push(skipped);
                        }
                        break;
                    }
                }
                messages.extend(image_notifications);
                *shared_state = context.shared_state.clone();

                cycles.push(cycle);
                let tool_boundary_result = observe_tool_batch_completion(
                    &budget_controller,
                    &controls,
                    cycle_index,
                    cancellation_token,
                    messages,
                    cycles,
                    shared_state,
                );
                if let Some(result) = checkpoint.update_budget_usage(
                    || budget_snapshot(&budget_controller),
                    messages,
                    cycles,
                    shared_state,
                ) {
                    return Some(result);
                }
                if let Some(result) = tool_boundary_result {
                    return Some(result);
                }
                finalize_tool_cycle(ToolCycleFinalization {
                    runtime: self,
                    controls: &controls,
                    task: &task,
                    cycle_index,
                    directive_result: directive_result.as_ref(),
                    completion_reason: directive_completion_reason,
                    completion_tool_name: directive_completion_tool_name.as_deref(),
                    messages,
                    cycles,
                    shared_state,
                    checkpoint: &checkpoint,
                    budget_controller: &budget_controller,
                    persisted_denials: &active_after_cycle_denials,
                })
            },
            effective_cancellation_token.as_ref(),
            cycle_index_start,
            cycle_count,
            effective_budget_limits,
            controls.initial_budget_usage.clone(),
            controls
                .checkpoint_controller
                .clone()
                .map(CheckpointRuntimeControl::into_controller),
        );
        if let Some(error) = checkpoint.take_llm_error() {
            return Err(error);
        }
        if let Some(error) = pending_error {
            return Err(error);
        }
        if backend_manages_checkpoint_cycles && !checkpoint.refresh_model_call_ledger()? {
            model_call_ledger
                .replace(result.token_usage.model_calls.clone())
                .map_err(LlmError::Request)?;
        }
        result.token_usage = model_call_ledger.usage();
        result = finalize_run_budget(
            &budget_controller,
            &controls,
            effective_cancellation_token.as_ref(),
            result,
        );
        result = finalize_terminal_projection(
            self,
            &controls,
            effective_cancellation_token.as_ref(),
            result,
        );
        Ok(result)
    }
}
