use super::*;
use crate::budget::BudgetUsageSnapshot;
use crate::runtime::engine::block_on_engine_tool_run;
use crate::tools::{ToolError, ToolLifecycleEvent};
use crate::types::{CompletionReason, ToolCall, ToolExecutionResult};

pub(super) fn execute_approval_resume_tool(
    invocation: &ApprovalResumeInvocation,
    controller: &CheckpointController,
    task: &AgentTask,
    run_context: &RunContext,
    event_handler: Option<crate::runtime::RunEventHandler>,
    registry: &ToolRegistry,
    budget_usage: Option<BudgetUsageSnapshot>,
) -> Result<(ToolExecutionResult, Option<CompletionReason>), String> {
    let approval = &invocation.approval;
    let target_cycle_index = 1;
    let target_idempotency =
        crate::runtime::run_definition::tool_idempotency_for(registry, &approval.call.name);
    if target_idempotency != approval.source_idempotency_support {
        return Err(
            "checkpoint_journal_integrity_mismatch: approved tool idempotency declaration changed"
                .to_string(),
        );
    }
    let plan = {
        let mut controller = controller
            .lock()
            .map_err(|_| "checkpoint_store_lock_poisoned: checkpoint controller lock poisoned")?;
        let (plan, interruption) = controller
            .plan_tool(
                target_cycle_index,
                &approval.call,
                target_idempotency,
                budget_usage.clone(),
                approval.source_request_digest.as_deref(),
                approval.source_idempotency_key.as_deref(),
            )
            .map_err(|error| error.to_string())?;
        if interruption.is_some() {
            return Err(
                "checkpoint_claim_active: approved tool target was interrupted before dispatch"
                    .to_string(),
            );
        }
        plan
    };
    let checkpoint_key = plan.checkpoint_key.clone().ok_or_else(|| {
        "checkpoint_journal_integrity_mismatch: target checkpoint identity missing".to_string()
    })?;
    let operation_id = plan.operation_id.clone().ok_or_else(|| {
        "checkpoint_journal_integrity_mismatch: target operation identity missing".to_string()
    })?;
    let attempt = plan.attempt.ok_or_else(|| {
        "checkpoint_journal_integrity_mismatch: target operation attempt missing".to_string()
    })?;
    let request_digest = plan.request_digest.clone().ok_or_else(|| {
        "checkpoint_journal_integrity_mismatch: target request digest missing".to_string()
    })?;
    let mut context = approval.context.clone();
    context.shared_state = invocation.source_result.shared_state.clone();
    context.cycle_index = target_cycle_index;
    context.task_id = task.task_id.clone();
    context.run_context = Some(run_context.clone());
    context.set_deferred_identity(checkpoint_key, operation_id, attempt, request_digest);

    let controller_for_dispatch = controller.clone();
    let before_dispatch = Arc::new(
        move |call: &ToolCall, _context: &mut crate::tools::ToolContext| {
            let outcome = controller_for_dispatch
                .lock()
                .map_err(|_| ToolError::new("checkpoint store lock poisoned"))
                .and_then(|mut controller| {
                    controller
                        .preflight_tool_dispatch(target_cycle_index, call)
                        .map_err(|error| ToolError::new(error.to_string()))?;
                    controller
                        .tool_started(target_cycle_index, call)
                        .map_err(|error| ToolError::new(error.to_string()))
                });
            outcome
        },
    );
    let lifecycle_handler = event_handler.clone();
    let lifecycle_run_id = run_context.run_id.clone();
    let lifecycle_trace_id = invocation.source_trace_id.clone();
    let lifecycle_agent_name = run_context.agent_name.clone();
    let lifecycle_callback = Arc::new(move |event: ToolLifecycleEvent| {
        if let Some(handler) = lifecycle_handler.as_ref() {
            handler(&super::super::resume::approval_lifecycle_run_event(
                event,
                &lifecycle_run_id,
                &lifecycle_trace_id,
                &lifecycle_agent_name,
                target_cycle_index,
            ));
        }
    });
    let options = approval
        .options
        .clone()
        .idempotency_key(plan.idempotency_key.clone())
        .before_dispatch(before_dispatch)
        .lifecycle_callback(lifecycle_callback);
    let mut execution = if let Some(result) = plan.replay_result {
        crate::tools::orchestrator::DeferredToolExecution::without_lifecycle(result)
    } else {
        block_on_engine_tool_run(
            approval
                .orchestrator
                .run_one_with_approval_and_metadata_deferred(
                    approval.call.clone(),
                    &mut context,
                    options,
                    |_call, _requirement, _context, _metadata| None,
                ),
        )
        .map_err(|error| error.to_string())?
    };
    let mut result = execution.result().clone();
    result = approval.hook_manager.apply_after_tool_call(
        task,
        target_cycle_index,
        &approval.call,
        &context,
        result,
    );
    let behavior_reason = crate::runtime::tool_call_runner::apply_tool_use_behavior(
        task,
        &approval.call,
        &mut result,
    );
    execution.replace_result(result);
    let durable_result = execution.result().clone();
    controller
        .lock()
        .map_err(|_| "checkpoint_store_lock_poisoned: checkpoint controller lock poisoned")?
        .finish_tool(
            target_cycle_index,
            &approval.call,
            &durable_result,
            budget_usage,
        )
        .map_err(|error| error.to_string())?;
    Ok((execution.complete(), behavior_reason))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run_approval_resume(
    invocation: &ApprovalResumeInvocation,
    controller: &CheckpointController,
    task: AgentTask,
    run_context: &RunContext,
    event_handler: Option<crate::runtime::RunEventHandler>,
    registry: &ToolRegistry,
    mut controls: RuntimeRunControls,
    runtime: &AgentRuntime<ArcLlmClient>,
) -> Result<AgentResult, String> {
    let (tool_result, behavior_reason) = execute_approval_resume_tool(
        invocation,
        controller,
        &task,
        run_context,
        event_handler,
        registry,
        controls.initial_budget_usage.clone(),
    )?;
    let mut initial_messages = invocation.source_result.messages.clone();
    initial_messages.retain(|message| {
        !(message.role == MessageRole::Tool
            && message.tool_call_id.as_deref() == Some(invocation.approval.call.id.as_str()))
    });
    initial_messages.push(tool_result.to_message());
    if tool_result.directive == crate::types::ToolDirective::Continue {
        controls.initial_messages = Some(initial_messages);
        controls.initial_shared_state = Some(invocation.source_result.shared_state.clone());
        runtime
            .run_with_controls(task, controls)
            .map_err(|error| error.to_string())
    } else {
        let mut resumed = invocation.source_result.clone();
        resumed.messages = initial_messages;
        resumed.shared_state = invocation.source_result.shared_state.clone();
        resumed.token_usage = crate::runtime::summarize_task_token_usage(
            controls.initial_model_calls.as_deref().unwrap_or_default(),
        );
        resumed.checkpoint_key = Some(
            controller
                .lock()
                .map_err(|_| "checkpoint_store_lock_poisoned: checkpoint controller lock poisoned")?
                .checkpoint_key()
                .map_err(|error| error.to_string())?
                .to_string(),
        );
        if let Some(cycle) = resumed
            .cycles
            .iter_mut()
            .find(|cycle| cycle.index == invocation.approval.cycle_index)
        {
            if let Some(existing) = cycle.tool_results.iter_mut().find(|existing| {
                existing.tool_call_id == invocation.approval.call.id
                    && existing
                        .metadata
                        .get("approval_interruption_id")
                        .and_then(Value::as_str)
                        == Some(invocation.approval.interruption_id.as_str())
            }) {
                *existing = tool_result.clone();
            } else {
                cycle.tool_results.push(tool_result.clone());
            }
        }
        resumed.completion_reason = behavior_reason.or(Some(match tool_result.directive {
            crate::types::ToolDirective::Finish => CompletionReason::ToolFinish,
            crate::types::ToolDirective::WaitUser => CompletionReason::WaitUser,
            crate::types::ToolDirective::Continue => unreachable!(),
        }));
        resumed.completion_tool_name = Some(invocation.approval.call.name.clone());
        resumed.error = None;
        match tool_result.directive {
            crate::types::ToolDirective::Finish => {
                resumed.status = AgentStatus::Completed;
                resumed.partial_output = None;
                resumed.final_answer = Some(crate::runtime::extract_final_message(&tool_result));
                resumed.wait_reason = None;
            }
            crate::types::ToolDirective::WaitUser => {
                resumed.status = AgentStatus::WaitUser;
                resumed.partial_output = crate::types::last_assistant_output(&resumed.cycles);
                resumed.final_answer = None;
                resumed.wait_reason = Some(crate::runtime::extract_wait_reason(&tool_result));
            }
            crate::types::ToolDirective::Continue => unreachable!(),
        }
        Ok(resumed)
    }
}
