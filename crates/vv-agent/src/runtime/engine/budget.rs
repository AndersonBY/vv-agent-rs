use std::collections::BTreeMap;

use serde_json::Value;

use crate::budget::{
    BudgetEnforcementBoundary, BudgetEvaluator, BudgetExhaustion, BudgetUsageSnapshot,
    RunBudgetLimits,
};
use crate::llm::LlmClient;
use crate::runtime::cancellation::CancellationToken;
use crate::runtime::token_usage::summarize_task_token_usage;
use crate::types::{
    last_assistant_output, AgentResult, AgentStatus, CompletionReason, CycleRecord, Message,
    Metadata, TokenUsage, ToolCall,
};

use super::helpers::{cancelled_agent_result, controls_cancelled};
use super::{AgentRuntime, RuntimeRunControls};

pub(super) struct PreparedRunBudget {
    pub(super) limits: Option<RunBudgetLimits>,
    pub(super) controller: Option<RunBudgetController>,
    pub(super) early_result: Option<AgentResult>,
}

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
        let mut controller = limits
            .clone()
            .filter(|_| !self.execution_backend.manages_run_budget())
            .map(|limits| {
                RunBudgetController::new(
                    BudgetEvaluator::new(
                        limits,
                        controls.host_cost_meter.clone(),
                        controls.initial_budget_usage.clone(),
                    )
                    .expect("validated configured run budget builds an evaluator"),
                )
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
            let mut result =
                cancelled_agent_result(messages.to_vec(), cycles.to_vec(), shared_state.clone());
            if let Some(controller) = &controller {
                result.budget_usage = Some(controller.snapshot());
            }
            Some(result)
        } else {
            controller.as_mut().and_then(|controller| {
                controller.run_start(controls).map(|exhaustion| {
                    budget_failure_result(
                        messages.to_vec(),
                        cycles.to_vec(),
                        shared_state.clone(),
                        controller,
                        exhaustion,
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
    controller: &mut Option<RunBudgetController>,
    controls: &RuntimeRunControls,
    cycle_index: u32,
    messages: &[Message],
    cycles: &[CycleRecord],
    shared_state: &Metadata,
) -> Option<AgentResult> {
    let controller = controller.as_mut()?;
    let exhaustion = controller.cycle_start(controls, cycle_index)?;
    Some(budget_failure_result(
        messages.to_vec(),
        cycles.to_vec(),
        shared_state.clone(),
        controller,
        exhaustion,
    ))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn observe_llm_completion(
    controller: &mut Option<RunBudgetController>,
    controls: &RuntimeRunControls,
    cycle_index: u32,
    token_usage: &TokenUsage,
    cancellation_token: Option<&CancellationToken>,
    cycle: &CycleRecord,
    messages: &[Message],
    cycles: &mut Vec<CycleRecord>,
    shared_state: &Metadata,
) -> Option<AgentResult> {
    let cancelled = cancellation_token.is_some_and(CancellationToken::is_cancelled)
        || controls_cancelled(controls);
    if let Some(controller) = controller {
        if let Some(exhaustion) =
            controller.llm_complete(controls, cycle_index, token_usage, cancelled)
        {
            cycles.push(cycle.clone());
            return Some(budget_failure_result(
                messages.to_vec(),
                cycles.clone(),
                shared_state.clone(),
                controller,
                exhaustion,
            ));
        }
    }
    if cancelled {
        cycles.push(cycle.clone());
        return Some(cancelled_agent_result(
            messages.to_vec(),
            cycles.clone(),
            shared_state.clone(),
        ));
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(super) fn preflight_tool_batch(
    controller: &mut Option<RunBudgetController>,
    controls: &RuntimeRunControls,
    cycle_index: u32,
    tool_calls: &[ToolCall],
    cycle: &CycleRecord,
    messages: &[Message],
    cycles: &mut Vec<CycleRecord>,
    shared_state: &Metadata,
) -> Option<AgentResult> {
    let controller = controller.as_mut()?;
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
        controller,
        exhaustion,
    ))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn observe_tool_batch_completion(
    controller: &mut Option<RunBudgetController>,
    controls: &RuntimeRunControls,
    cycle_index: u32,
    cancellation_token: Option<&CancellationToken>,
    messages: &[Message],
    cycles: &[CycleRecord],
    shared_state: &Metadata,
) -> Option<AgentResult> {
    let cancelled = cancellation_token.is_some_and(CancellationToken::is_cancelled)
        || controls_cancelled(controls);
    if let Some(controller) = controller {
        if let Some(exhaustion) =
            controller.tool_batch_complete(controls, cycle_index, false, cancelled)
        {
            return Some(budget_failure_result(
                messages.to_vec(),
                cycles.to_vec(),
                shared_state.clone(),
                controller,
                exhaustion,
            ));
        }
    }
    cancelled
        .then(|| cancelled_agent_result(messages.to_vec(), cycles.to_vec(), shared_state.clone()))
}

pub(super) fn finalize_run_budget(
    controller: &mut Option<RunBudgetController>,
    controls: &RuntimeRunControls,
    cancellation_token: Option<&CancellationToken>,
    mut result: AgentResult,
) -> AgentResult {
    let Some(controller) = controller else {
        return result;
    };
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
                    controller,
                    exhaustion,
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

    pub(super) fn llm_complete(
        &mut self,
        controls: &RuntimeRunControls,
        cycle_index: u32,
        usage: &crate::types::TokenUsage,
        suppress_exhaustion: bool,
    ) -> Option<BudgetExhaustion> {
        self.observe(
            controls,
            BudgetEnforcementBoundary::LlmComplete,
            Some(cycle_index),
            false,
            suppress_exhaustion,
            |evaluator| evaluator.llm_complete(usage),
        )
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
        if let Some(exhaustion) = &self.exhaustion {
            return Some(exhaustion.clone());
        }
        let exhaustion = operation(&mut self.evaluator);
        let snapshot = self.evaluator.snapshot();
        if let Some(exhaustion) = exhaustion.filter(|_| !suppress_exhaustion) {
            self.exhaustion = Some(exhaustion.clone());
            emit_budget_log(
                controls,
                "budget_exhausted",
                boundary,
                cycle_index,
                &snapshot,
                Some(&exhaustion),
            );
            self.last_emitted_snapshot = Some(snapshot);
            return Some(exhaustion);
        }
        if force_snapshot || self.last_emitted_snapshot.as_ref() != Some(&snapshot) {
            emit_budget_log(
                controls,
                "budget_snapshot",
                boundary,
                cycle_index,
                &snapshot,
                None,
            );
            self.last_emitted_snapshot = Some(snapshot);
        }
        None
    }
}

pub(super) fn budget_failure_result(
    messages: Vec<Message>,
    cycles: Vec<CycleRecord>,
    shared_state: Metadata,
    controller: &RunBudgetController,
    exhaustion: BudgetExhaustion,
) -> AgentResult {
    let token_usage = summarize_task_token_usage(&cycles);
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
    let Some(handler) = &controls.log_handler else {
        return;
    };
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
    handler(event, &payload);
}
