use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::Value;

use crate::budget::{
    BudgetEnforcementBoundary, BudgetEvaluator, BudgetExhaustion, BudgetUsageSnapshot,
    RunBudgetLimits,
};
use crate::llm::LlmClient;
use crate::runtime::cancellation::CancellationToken;
use crate::runtime::model_calls::ModelCallBudgetUpdate;
use crate::types::{
    last_assistant_output, AgentResult, AgentStatus, CompletionReason, CycleRecord, Message,
    Metadata, TaskTokenUsage, ToolCall,
};

use super::helpers::{cancelled_agent_result, controls_cancelled, task_token_usage};
use super::{AgentRuntime, RuntimeRunControls};

pub(super) struct PreparedRunBudget {
    pub(super) limits: Option<RunBudgetLimits>,
    pub(super) controller: Option<SharedRunBudgetController>,
    pub(super) early_result: Option<AgentResult>,
}

pub(super) type SharedRunBudgetController = Arc<Mutex<RunBudgetController>>;

impl<C: LlmClient + Clone + 'static> AgentRuntime<C> {
    pub(super) fn prepare_run_budget(
        &self,
        controls: &RuntimeRunControls,
        messages: &[Message],
        cycles: &[CycleRecord],
        shared_state: &Metadata,
    ) -> PreparedRunBudget {
        let limits = controls
            .budget_limits
            .clone()
            .filter(RunBudgetLimits::has_limits);
        let controller = limits
            .clone()
            .filter(|_| !self.execution_backend.manages_run_budget())
            .map(|limits| {
                Arc::new(Mutex::new(RunBudgetController::new(
                    BudgetEvaluator::new(
                        limits,
                        controls.host_cost_meter.clone(),
                        controls.initial_budget_usage.clone(),
                    )
                    .expect("validated configured run budget builds an evaluator"),
                )))
            });

        let early_result = if limits.is_some() && controls_cancelled(controls) {
            self.emit_log(
                controls,
                "run_cancelled",
                BTreeMap::from([(
                    "error".to_string(),
                    Value::String("Operation was cancelled".to_string()),
                )]),
            );
            let mut result = cancelled_agent_result(
                messages.to_vec(),
                cycles.to_vec(),
                shared_state.clone(),
                task_token_usage(controls),
            );
            if let Some(controller) = &controller {
                result.budget_usage = Some(lock_budget(controller).snapshot());
            }
            Some(result)
        } else {
            controller.as_ref().and_then(|controller| {
                let mut controller = lock_budget(controller);
                controller.run_start(controls).map(|exhaustion| {
                    budget_failure_result(
                        messages.to_vec(),
                        cycles.to_vec(),
                        shared_state.clone(),
                        &controller,
                        exhaustion,
                        task_token_usage(controls),
                    )
                })
            })
        };

        PreparedRunBudget {
            limits,
            controller,
            early_result,
        }
    }
}

pub(super) fn enforce_cycle_start(
    controller: &Option<SharedRunBudgetController>,
    controls: &RuntimeRunControls,
    cycle_index: u32,
    messages: &[Message],
    cycles: &[CycleRecord],
    shared_state: &Metadata,
) -> Option<AgentResult> {
    let mut controller = lock_budget(controller.as_ref()?);
    let exhaustion = controller.cycle_start(controls, cycle_index)?;
    Some(budget_failure_result(
        messages.to_vec(),
        cycles.to_vec(),
        shared_state.clone(),
        &controller,
        exhaustion,
        task_token_usage(controls),
    ))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn project_model_call_completion(
    controller: &Option<SharedRunBudgetController>,
    controls: &RuntimeRunControls,
    budget_exhaustion: Option<BudgetExhaustion>,
    cancellation_token: Option<&CancellationToken>,
    cycle: &CycleRecord,
    messages: &[Message],
    cycles: &mut Vec<CycleRecord>,
    shared_state: &Metadata,
) -> Option<AgentResult> {
    let cancelled = cancellation_token.is_some_and(CancellationToken::is_cancelled)
        || controls_cancelled(controls);
    if let Some(exhaustion) = budget_exhaustion {
        let controller = controller
            .as_ref()
            .expect("model-call exhaustion requires a budget controller");
        let controller = lock_budget(controller);
        cycles.push(cycle.clone());
        return Some(budget_failure_result(
            messages.to_vec(),
            cycles.clone(),
            shared_state.clone(),
            &controller,
            exhaustion,
            task_token_usage(controls),
        ));
    }
    if cancelled {
        cycles.push(cycle.clone());
        return Some(cancelled_agent_result(
            messages.to_vec(),
            cycles.clone(),
            shared_state.clone(),
            task_token_usage(controls),
        ));
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(super) fn preflight_tool_batch(
    controller: &Option<SharedRunBudgetController>,
    controls: &RuntimeRunControls,
    cycle_index: u32,
    tool_calls: &[ToolCall],
    cycle: &CycleRecord,
    messages: &[Message],
    cycles: &mut Vec<CycleRecord>,
    shared_state: &Metadata,
) -> Option<AgentResult> {
    let mut controller = lock_budget(controller.as_ref()?);
    let tool_names = tool_calls
        .iter()
        .map(|call| call.name.clone())
        .collect::<Vec<_>>();
    let exhaustion = controller.preflight_tools(controls, cycle_index, &tool_names)?;
    cycles.push(cycle.clone());
    Some(budget_failure_result(
        messages.to_vec(),
        cycles.clone(),
        shared_state.clone(),
        &controller,
        exhaustion,
        task_token_usage(controls),
    ))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn observe_tool_batch_completion(
    controller: &Option<SharedRunBudgetController>,
    controls: &RuntimeRunControls,
    cycle_index: u32,
    cancellation_token: Option<&CancellationToken>,
    messages: &[Message],
    cycles: &[CycleRecord],
    shared_state: &Metadata,
) -> Option<AgentResult> {
    let cancelled = cancellation_token.is_some_and(CancellationToken::is_cancelled)
        || controls_cancelled(controls);
    if let Some(controller) = controller.as_ref() {
        let mut controller = lock_budget(controller);
        if let Some(exhaustion) =
            controller.tool_batch_complete(controls, cycle_index, false, cancelled)
        {
            return Some(budget_failure_result(
                messages.to_vec(),
                cycles.to_vec(),
                shared_state.clone(),
                &controller,
                exhaustion,
                task_token_usage(controls),
            ));
        }
    }
    cancelled.then(|| {
        cancelled_agent_result(
            messages.to_vec(),
            cycles.to_vec(),
            shared_state.clone(),
            task_token_usage(controls),
        )
    })
}

pub(super) fn finalize_run_budget(
    controller: &Option<SharedRunBudgetController>,
    controls: &RuntimeRunControls,
    cancellation_token: Option<&CancellationToken>,
    mut result: AgentResult,
) -> AgentResult {
    let Some(controller) = controller.as_ref() else {
        return result;
    };
    let mut controller = lock_budget(controller);
    let cancelled = result.status == AgentStatus::Failed
        && cancellation_token.is_some_and(CancellationToken::is_cancelled);
    let operation_failed = result.status == AgentStatus::Failed
        && !matches!(
            result.completion_reason,
            Some(CompletionReason::BudgetExhausted | CompletionReason::Cancelled)
        );
    let deferred_segment =
        controls.defer_terminal_on_max_cycles && result.status == AgentStatus::MaxCycles;
    if controller.exhaustion().is_none() && !deferred_segment {
        if let Some(exhaustion) = controller.terminal(controls, cancelled || operation_failed) {
            if !cancelled && !operation_failed {
                result = budget_failure_result(
                    result.messages,
                    result.cycles,
                    result.shared_state,
                    &controller,
                    exhaustion,
                    result.token_usage,
                );
            }
        }
    }
    result.budget_usage = Some(controller.snapshot());
    result.budget_exhaustion = controller.exhaustion().cloned();
    result
}

pub(super) struct RunBudgetController {
    evaluator: BudgetEvaluator,
    exhaustion: Option<BudgetExhaustion>,
    last_emitted_snapshot: Option<BudgetUsageSnapshot>,
}

impl RunBudgetController {
    pub(super) fn new(evaluator: BudgetEvaluator) -> Self {
        Self {
            evaluator,
            exhaustion: None,
            last_emitted_snapshot: None,
        }
    }

    pub(super) fn snapshot(&self) -> BudgetUsageSnapshot {
        self.evaluator.snapshot()
    }

    pub(super) fn exhaustion(&self) -> Option<&BudgetExhaustion> {
        self.exhaustion.as_ref()
    }

    pub(super) fn run_start(&mut self, controls: &RuntimeRunControls) -> Option<BudgetExhaustion> {
        self.observe(
            controls,
            BudgetEnforcementBoundary::RunStart,
            None,
            true,
            false,
            BudgetEvaluator::run_start,
        )
    }

    pub(super) fn cycle_start(
        &mut self,
        controls: &RuntimeRunControls,
        cycle_index: u32,
    ) -> Option<BudgetExhaustion> {
        self.observe(
            controls,
            BudgetEnforcementBoundary::CycleStart,
            Some(cycle_index),
            false,
            false,
            BudgetEvaluator::cycle_start,
        )
    }

    pub(super) fn model_call_complete(
        &mut self,
        cycle_index: u32,
        usage: &crate::types::TokenUsage,
        suppress_exhaustion: bool,
    ) -> ModelCallBudgetUpdate {
        let (exhaustion, snapshot) = self.observe_update(
            BudgetEnforcementBoundary::ModelCallComplete,
            Some(cycle_index),
            false,
            suppress_exhaustion,
            |evaluator| evaluator.model_call_complete(usage),
        );
        ModelCallBudgetUpdate {
            exhaustion,
            snapshot,
        }
    }

    pub(super) fn preflight_tools(
        &mut self,
        controls: &RuntimeRunControls,
        cycle_index: u32,
        tool_names: &[String],
    ) -> Option<BudgetExhaustion> {
        self.observe(
            controls,
            BudgetEnforcementBoundary::ToolBatchPreflight,
            Some(cycle_index),
            false,
            false,
            |evaluator| evaluator.preflight_tools(tool_names),
        )
    }

    pub(super) fn tool_batch_complete(
        &mut self,
        controls: &RuntimeRunControls,
        cycle_index: u32,
        operation_failed: bool,
        suppress_exhaustion: bool,
    ) -> Option<BudgetExhaustion> {
        self.observe(
            controls,
            BudgetEnforcementBoundary::ToolBatchComplete,
            Some(cycle_index),
            false,
            operation_failed || suppress_exhaustion,
            |evaluator| evaluator.tool_batch_complete(operation_failed),
        )
    }

    pub(super) fn terminal(
        &mut self,
        controls: &RuntimeRunControls,
        suppress_exhaustion: bool,
    ) -> Option<BudgetExhaustion> {
        self.observe(
            controls,
            BudgetEnforcementBoundary::Terminal,
            None,
            false,
            suppress_exhaustion,
            BudgetEvaluator::terminal,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn observe(
        &mut self,
        controls: &RuntimeRunControls,
        boundary: BudgetEnforcementBoundary,
        cycle_index: Option<u32>,
        force_snapshot: bool,
        suppress_exhaustion: bool,
        operation: impl FnOnce(&mut BudgetEvaluator) -> Option<BudgetExhaustion>,
    ) -> Option<BudgetExhaustion> {
        let (exhaustion, snapshot) = self.observe_update(
            boundary,
            cycle_index,
            force_snapshot,
            suppress_exhaustion,
            operation,
        );
        if let Some(snapshot) = snapshot {
            emit_budget_log(
                controls,
                if exhaustion.is_some() {
                    "budget_exhausted"
                } else {
                    "budget_snapshot"
                },
                boundary,
                cycle_index,
                &snapshot,
                exhaustion.as_ref(),
            );
        }
        exhaustion
    }

    fn observe_update(
        &mut self,
        _boundary: BudgetEnforcementBoundary,
        _cycle_index: Option<u32>,
        force_snapshot: bool,
        suppress_exhaustion: bool,
        operation: impl FnOnce(&mut BudgetEvaluator) -> Option<BudgetExhaustion>,
    ) -> (Option<BudgetExhaustion>, Option<BudgetUsageSnapshot>) {
        if let Some(exhaustion) = &self.exhaustion {
            return (Some(exhaustion.clone()), None);
        }
        let exhaustion = operation(&mut self.evaluator);
        let snapshot = self.evaluator.snapshot();
        if let Some(exhaustion) = exhaustion.filter(|_| !suppress_exhaustion) {
            self.exhaustion = Some(exhaustion.clone());
            self.last_emitted_snapshot = Some(snapshot.clone());
            return (Some(exhaustion), Some(snapshot));
        }
        if force_snapshot || self.last_emitted_snapshot.as_ref() != Some(&snapshot) {
            self.last_emitted_snapshot = Some(snapshot.clone());
            return (None, Some(snapshot));
        }
        (None, None)
    }
}

pub(super) fn lock_budget(
    controller: &SharedRunBudgetController,
) -> MutexGuard<'_, RunBudgetController> {
    controller
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(super) fn budget_snapshot(
    controller: &Option<SharedRunBudgetController>,
) -> Option<BudgetUsageSnapshot> {
    controller
        .as_ref()
        .map(|controller| lock_budget(controller).snapshot())
}

pub(super) fn budget_failure_result(
    messages: Vec<Message>,
    cycles: Vec<CycleRecord>,
    shared_state: Metadata,
    controller: &RunBudgetController,
    exhaustion: BudgetExhaustion,
    token_usage: TaskTokenUsage,
) -> AgentResult {
    let partial_output = last_assistant_output(&cycles);
    AgentResult {
        status: AgentStatus::Failed,
        messages,
        cycles,
        completion_reason: Some(CompletionReason::BudgetExhausted),
        completion_tool_name: None,
        partial_output,
        budget_usage: Some(controller.snapshot()),
        budget_exhaustion: Some(exhaustion),
        checkpoint_key: None,
        resume_observation: None,
        final_answer: None,
        wait_reason: None,
        error: Some("Run budget exhausted.".to_string()),
        error_code: None,
        shared_state,
        token_usage,
    }
}

fn emit_budget_log(
    controls: &RuntimeRunControls,
    event: &str,
    boundary: BudgetEnforcementBoundary,
    cycle_index: Option<u32>,
    snapshot: &BudgetUsageSnapshot,
    exhaustion: Option<&BudgetExhaustion>,
) {
    let mut payload = BTreeMap::from([
        (
            "enforcement_boundary".to_string(),
            serde_json::to_value(boundary).expect("budget boundary serializes"),
        ),
        (
            "budget_usage".to_string(),
            serde_json::to_value(snapshot).expect("budget snapshot serializes"),
        ),
    ]);
    if let Some(cycle_index) = cycle_index {
        payload.insert("cycle".to_string(), Value::from(cycle_index));
    }
    if let Some(exhaustion) = exhaustion {
        payload.insert(
            "budget_exhaustion".to_string(),
            serde_json::to_value(exhaustion).expect("budget exhaustion serializes"),
        );
    }
    super::logging::emit_runtime_event(
        None,
        controls.event_handler.as_ref(),
        controls.execution_context.as_ref(),
        event,
        payload,
    );
}
