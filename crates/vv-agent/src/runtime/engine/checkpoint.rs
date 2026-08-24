use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::Value;

use crate::budget::BudgetUsageSnapshot;
use crate::checkpoint::{
    CheckpointError, CheckpointResult, DeferredBatchEntry, ToolCallOutcome, ToolIdempotency,
};
use crate::llm::{LlmError, LlmRequest};
use crate::runtime::checkpoint_resume::{
    CheckpointController, CheckpointResumeController, ModelOperationOutcome, ToolOperationPlan,
};
use crate::tools::{
    orchestrator::DeferredToolExecution, BeforeToolDispatch, ToolError, ToolRegistry,
    ToolRunOptions,
};
use crate::types::{AgentResult, CycleRecord, LLMResponse, Message, ToolCall, ToolExecutionResult};

use super::helpers::failed_agent_result;
use crate::runtime::model_calls::{
    ModelCallCoordinator, ModelCallDispatchRequest, ModelCallDispatchResult, ModelCallLedger,
};

type PendingCheckpointError = Arc<Mutex<Option<CheckpointError>>>;
type FailureContext<'a> = (
    &'a [Message],
    &'a [CycleRecord],
    &'a BTreeMap<String, Value>,
);

pub(super) enum CheckpointModelCompletion {
    Continue(Box<Result<ModelCallDispatchResult, LlmError>>),
    Stop(Box<AgentResult>),
}

pub(super) enum CheckpointModelDispatch {
    Continue(Box<Result<ModelCallDispatchResult, LlmError>>),
    Interrupted(Box<AgentResult>),
    Failed(CheckpointError),
}

pub(super) enum CheckpointToolPlan {
    Continue(Option<Box<ToolOperationPlan>>),
    Stop(Box<AgentResult>),
}

#[derive(Clone)]
pub(super) struct CheckpointCoordinator {
    controller: Option<CheckpointController>,
    pending_error: PendingCheckpointError,
    model_call_ledger: ModelCallLedger,
}

pub(super) struct DeferredBatchCollector<'a> {
    checkpoint: &'a CheckpointCoordinator,
    registry: &'a ToolRegistry,
    cycle_index: u32,
    entries: Vec<DeferredBatchEntry>,
    // Lifecycle callbacks are staged with the same ownership boundary as the
    // journal entries.  A provider may have already produced an outcome, but
    // no deferred/completed observation is externally visible until the
    // admission CAS (and its outbox write) succeeds.
    lifecycle: Vec<DeferredToolExecution>,
}

impl<'a> DeferredBatchCollector<'a> {
    pub(super) fn new(
        checkpoint: &'a CheckpointCoordinator,
        registry: &'a ToolRegistry,
        cycle_index: u32,
    ) -> Self {
        Self {
            checkpoint,
            registry,
            cycle_index,
            entries: Vec::new(),
            lifecycle: Vec::new(),
        }
    }

    pub(super) fn capture(
        &mut self,
        call: &ToolCall,
        plan: Option<&ToolOperationPlan>,
        outcome: ToolCallOutcome,
    ) -> bool {
        let deferred = matches!(outcome, ToolCallOutcome::Deferred { .. });
        self.checkpoint.append_deferred_batch_entry(
            &mut self.entries,
            self.cycle_index,
            call,
            plan,
            self.registry,
            outcome,
        );
        deferred
    }

    pub(super) fn capture_or_return_execution(
        &mut self,
        call: &ToolCall,
        plan: Option<&ToolOperationPlan>,
        execution: DeferredToolExecution,
    ) -> Option<DeferredToolExecution> {
        let outcome = execution.outcome().clone();
        if !matches!(outcome, ToolCallOutcome::Deferred { .. }) {
            return Some(execution);
        }
        self.capture(call, plan, outcome);
        self.lifecycle.push(execution);
        None
    }

    pub(super) fn capture_completed_execution(
        &mut self,
        call: &ToolCall,
        plan: Option<&ToolOperationPlan>,
        execution: DeferredToolExecution,
    ) -> ToolExecutionResult {
        let result = execution.result().clone();
        let entry_count = self.entries.len();
        self.capture(call, plan, ToolCallOutcome::completed(result.clone()));
        if self.entries.len() != entry_count {
            self.lifecycle.push(execution);
        } else {
            // The collector is also used by the non-checkpoint runtime. In
            // that mode there is no CAS boundary to stage behind, so preserve
            // the normal immediate lifecycle callback ordering.
            let _ = execution.complete();
        }
        result
    }

    pub(super) fn complete_lifecycle(&mut self) {
        for execution in self.lifecycle.drain(..) {
            let _ = execution.complete();
        }
    }

    pub(super) fn finish(
        &mut self,
        messages: &[Message],
        cycles: &[CycleRecord],
        shared_state: &BTreeMap<String, Value>,
    ) -> Option<AgentResult> {
        let has_deferred = self
            .entries
            .iter()
            .any(|entry| entry.outcome.handle().is_some());
        let result =
            self.checkpoint
                .finish_tool_batch(&self.entries, messages, cycles, shared_state);
        if has_deferred {
            // Deferred admission writes the canonical lifecycle events into
            // the durable outbox in the same CAS.  Replaying the staged
            // callbacks here would emit a second deferred/completed event
            // for the same operation, and would also leak an event if a
            // later projection crashes.  The outbox is the sole authority
            // once a batch contains a deferred outcome; clear callbacks on
            // both success and admission failure.
            self.lifecycle.clear();
        } else if result
            .as_ref()
            .is_none_or(|result| matches!(result.status, crate::types::AgentStatus::Deferred))
        {
            self.complete_lifecycle();
        }
        result
    }
}

impl CheckpointCoordinator {
    pub(super) fn new(
        controller: Option<CheckpointController>,
        model_call_ledger: ModelCallLedger,
    ) -> Self {
        Self {
            controller,
            pending_error: Arc::new(Mutex::new(None)),
            model_call_ledger,
        }
    }

    pub(super) fn begin_run_cycle(
        &self,
        cycle_index: u32,
    ) -> Result<Option<AgentResult>, LlmError> {
        match self.operation(|controller| controller.begin_cycle(cycle_index)) {
            Some(result) => result.map_err(checkpoint_llm_error),
            None => Ok(None),
        }
    }

    pub(super) fn bind_model_accounting(
        &self,
        accounting: &ModelCallCoordinator,
    ) -> Result<(), LlmError> {
        let Some(controller) = self.controller.as_ref() else {
            return Ok(());
        };
        lock_controller(controller)
            .map(|mut controller| controller.bind_model_accounting(accounting.clone()))
            .map_err(checkpoint_llm_error)
    }

    pub(super) fn refresh_model_call_ledger(&self) -> Result<bool, LlmError> {
        let Some(controller) = self.controller.as_ref() else {
            return Ok(false);
        };
        let model_calls = lock_controller(controller)
            .and_then(|mut controller| controller.refresh_authoritative())
            .map_err(checkpoint_llm_error)?
            .model_calls;
        self.model_call_ledger
            .replace(model_calls)
            .map_err(LlmError::Request)?;
        Ok(true)
    }

    pub(super) fn begin_cycle(
        &self,
        cycle_index: u32,
        messages: &[Message],
        cycles: &[CycleRecord],
        shared_state: &BTreeMap<String, Value>,
    ) -> Option<AgentResult> {
        match self.operation(|controller| controller.begin_cycle(cycle_index)) {
            Some(Ok(result)) => result,
            Some(Err(error)) => Some(self.failure(error, messages, cycles, shared_state)),
            None => None,
        }
    }

    pub(super) fn update_budget_usage<F>(
        &self,
        budget_usage: F,
        messages: &[Message],
        cycles: &[CycleRecord],
        shared_state: &BTreeMap<String, Value>,
    ) -> Option<AgentResult>
    where
        F: FnOnce() -> Option<BudgetUsageSnapshot>,
    {
        let controller = self.controller.as_ref()?;
        let budget_usage = budget_usage();
        let outcome = lock_controller(controller)
            .and_then(|mut controller| controller.update_budget_usage(budget_usage));
        match outcome {
            Err(error) => Some(self.failure(error, messages, cycles, shared_state)),
            Ok(()) => None,
        }
    }

    pub(super) fn complete_model<F, B>(
        &self,
        dispatch: ModelCallDispatchRequest<'_>,
        budget_usage: B,
        invoke: F,
        failure_context: FailureContext<'_>,
    ) -> CheckpointModelCompletion
    where
        F: FnOnce(LlmRequest) -> Result<LLMResponse, LlmError>,
        B: FnOnce() -> Option<BudgetUsageSnapshot>,
    {
        match self.dispatch_model(dispatch, budget_usage, invoke) {
            CheckpointModelDispatch::Continue(completion) => {
                CheckpointModelCompletion::Continue(completion)
            }
            CheckpointModelDispatch::Interrupted(result) => CheckpointModelCompletion::Stop(result),
            CheckpointModelDispatch::Failed(error) => {
                CheckpointModelCompletion::Stop(Box::new(self.failure(
                    error,
                    failure_context.0,
                    failure_context.1,
                    failure_context.2,
                )))
            }
        }
    }

    pub(super) fn dispatch_model<F, B>(
        &self,
        dispatch: ModelCallDispatchRequest<'_>,
        budget_usage: B,
        invoke: F,
    ) -> CheckpointModelDispatch
    where
        F: FnOnce(LlmRequest) -> Result<LLMResponse, LlmError>,
        B: FnOnce() -> Option<BudgetUsageSnapshot>,
    {
        let Some(controller) = self.controller.as_ref() else {
            return CheckpointModelDispatch::Continue(Box::new(dispatch.accounting.dispatch(
                dispatch.operation,
                dispatch.cycle_index,
                dispatch.operation_slot,
                dispatch.backend,
                dispatch.model,
                dispatch.request,
                || invoke(dispatch.request.clone()),
            )));
        };
        let invoke_request = dispatch.request.clone();
        let budget_usage = budget_usage();
        let outcome = lock_controller(controller).and_then(|mut controller| {
            controller.complete_model(dispatch, budget_usage, || invoke(invoke_request))
        });
        match outcome {
            Ok(ModelOperationOutcome::Response(response)) => {
                CheckpointModelDispatch::Continue(Box::new(Ok(*response)))
            }
            Ok(ModelOperationOutcome::Error(error)) => {
                CheckpointModelDispatch::Continue(Box::new(Err(error)))
            }
            Ok(ModelOperationOutcome::Interrupted(result)) => {
                CheckpointModelDispatch::Interrupted(result)
            }
            Err(error) => CheckpointModelDispatch::Failed(error),
        }
    }

    pub(super) fn failure_result(
        &self,
        error: CheckpointError,
        messages: &[Message],
        cycles: &[CycleRecord],
        shared_state: &BTreeMap<String, Value>,
    ) -> AgentResult {
        self.failure(error, messages, cycles, shared_state)
    }

    pub(super) fn plan_tool<F>(
        &self,
        cycle_index: u32,
        call: &ToolCall,
        operation_inputs: F,
        messages: &[Message],
        cycles: &[CycleRecord],
        shared_state: &BTreeMap<String, Value>,
    ) -> CheckpointToolPlan
    where
        F: FnOnce() -> (ToolIdempotency, Option<BudgetUsageSnapshot>),
    {
        let Some(controller) = self.controller.as_ref() else {
            return CheckpointToolPlan::Continue(None);
        };
        let (idempotency, budget_usage) = operation_inputs();
        let outcome = lock_controller(controller).and_then(|mut controller| {
            controller.plan_tool(cycle_index, call, idempotency, budget_usage)
        });
        match outcome {
            Ok((_plan, Some(result))) => CheckpointToolPlan::Stop(Box::new(result)),
            Ok((plan, None)) => CheckpointToolPlan::Continue(Some(Box::new(plan))),
            Err(error) => CheckpointToolPlan::Stop(Box::new(self.failure(
                error,
                messages,
                cycles,
                shared_state,
            ))),
        }
    }

    pub(super) fn set_tool_context_identity(
        &self,
        context: &mut crate::tools::ToolContext,
        cycle_index: u32,
        call: &ToolCall,
    ) {
        context.clear_deferred_identity();
        let Some(controller) = self.controller.as_ref() else {
            return;
        };
        let Ok(controller) = lock_controller(controller) else {
            return;
        };
        if let Some((checkpoint_key, operation_id, attempt, request_digest)) =
            controller.deferred_tool_identity(cycle_index, &call.id)
        {
            context.set_deferred_identity(checkpoint_key, operation_id, attempt, request_digest);
        }
    }

    pub(super) fn tool_identity(
        &self,
        cycle_index: u32,
        call: &ToolCall,
    ) -> Option<(String, String, u64, String)> {
        let controller = self.controller.as_ref()?;
        let controller = lock_controller(controller).ok()?;
        controller.deferred_tool_identity(cycle_index, &call.id)
    }

    pub(super) fn deferred_batch_entry(
        &self,
        cycle_index: u32,
        call: &ToolCall,
        plan: Option<&ToolOperationPlan>,
        registry: &ToolRegistry,
        outcome: ToolCallOutcome,
    ) -> Option<DeferredBatchEntry> {
        let (_, operation_id, attempt, request_digest) = self.tool_identity(cycle_index, call)?;
        Some(DeferredBatchEntry {
            operation_id,
            cycle_index: u64::from(cycle_index),
            attempt,
            request_digest,
            tool_call_id: call.id.clone(),
            tool_name: call.name.clone(),
            idempotency_key: plan.and_then(|plan| plan.idempotency_key.clone()),
            idempotency_support: crate::runtime::run_definition::tool_idempotency_for(
                registry, &call.name,
            ),
            outcome,
        })
    }

    pub(super) fn append_deferred_batch_entry(
        &self,
        entries: &mut Vec<DeferredBatchEntry>,
        cycle_index: u32,
        call: &ToolCall,
        plan: Option<&ToolOperationPlan>,
        registry: &ToolRegistry,
        outcome: ToolCallOutcome,
    ) {
        if let Some(entry) = self.deferred_batch_entry(cycle_index, call, plan, registry, outcome) {
            entries.push(entry);
        }
    }

    pub(super) fn finish_tool_batch(
        &self,
        entries: &[crate::checkpoint::DeferredBatchEntry],
        messages: &[Message],
        cycles: &[CycleRecord],
        shared_state: &BTreeMap<String, Value>,
    ) -> Option<AgentResult> {
        if entries.is_empty() {
            return None;
        }
        let has_deferred = entries.iter().any(|entry| entry.outcome.handle().is_some());
        if has_deferred {
            let controller = self.controller.as_ref()?;
            let result = lock_controller(controller).and_then(|mut controller| {
                controller.admit_deferred_batch(entries)?;
                controller.deferred_result(messages, cycles, shared_state)
            });
            return match result {
                Ok(result) => Some(result),
                Err(error) => Some(self.failure(error, messages, cycles, shared_state)),
            };
        }
        // Ordinary batches retain their existing per-tool journal path. This
        // keeps approval/short-circuit semantics unchanged when no deferred
        // outcome is present.
        for entry in entries {
            let Some(controller) = self.controller.as_ref() else {
                break;
            };
            let Some(result) = entry.outcome.result() else {
                continue;
            };
            let outcome = lock_controller(controller).and_then(|mut controller| {
                controller.finish_tool(
                    entry.cycle_index as u32,
                    &ToolCall::new(
                        entry.tool_call_id.clone(),
                        entry.tool_name.clone(),
                        crate::types::ToolArguments::new(),
                    ),
                    result,
                    None,
                )
            });
            if let Err(error) = outcome {
                return Some(self.failure(error, messages, cycles, shared_state));
            }
        }
        None
    }

    pub(super) fn before_tool_dispatch(
        &self,
        options: ToolRunOptions,
        cycle_index: u32,
    ) -> ToolRunOptions {
        let Some(controller) = self.controller.as_ref() else {
            return options;
        };
        let controller = controller.clone();
        let pending_error = self.pending_error.clone();
        let callback: BeforeToolDispatch = Arc::new(move |call, _context| {
            let outcome = lock_controller(&controller).and_then(|mut controller| {
                controller.preflight_tool_dispatch(cycle_index, call)?;
                controller.tool_started(cycle_index, call)
            });
            match outcome {
                Ok(()) => Ok(()),
                Err(error) => {
                    *pending_error
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error.clone());
                    Err(ToolError::new(format!(
                        "{}: {}",
                        error.code(),
                        error.message()
                    )))
                }
            }
        });
        options.before_dispatch(callback)
    }

    pub(super) fn pending_failure(
        &self,
        messages: &[Message],
        cycles: &[CycleRecord],
        shared_state: &BTreeMap<String, Value>,
    ) -> Option<AgentResult> {
        self.pending_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
            .then(|| {
                checkpoint_failed_result(messages, cycles, shared_state, &self.model_call_ledger)
            })
    }

    pub(super) fn finish_tool<F>(
        &self,
        cycle_index: u32,
        call: &ToolCall,
        result: &ToolExecutionResult,
        budget_usage: F,
        failure_context: FailureContext<'_>,
    ) -> Option<AgentResult>
    where
        F: FnOnce() -> Option<BudgetUsageSnapshot>,
    {
        let controller = self.controller.as_ref()?;
        let budget_usage = budget_usage();
        let outcome = lock_controller(controller).and_then(|mut controller| {
            controller.finish_tool(cycle_index, call, result, budget_usage)
        });
        match outcome {
            Ok(result) => result,
            Err(error) => Some(self.failure(
                error,
                failure_context.0,
                failure_context.1,
                failure_context.2,
            )),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn commit_cycle<F>(
        &self,
        cycle_index: u32,
        messages: &[Message],
        cycles: &[CycleRecord],
        shared_state: &BTreeMap<String, Value>,
        budget_usage: F,
    ) -> Option<AgentResult>
    where
        F: FnOnce() -> Option<BudgetUsageSnapshot>,
    {
        let controller = self.controller.as_ref()?;
        let budget_usage = budget_usage();
        let outcome = lock_controller(controller).and_then(|mut controller| {
            controller.commit_cycle(cycle_index, messages, cycles, shared_state, budget_usage)
        });
        match outcome {
            Err(error) => Some(self.failure(error, messages, cycles, shared_state)),
            Ok(()) => None,
        }
    }

    pub(super) fn take_llm_error(&self) -> Option<LlmError> {
        self.pending_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .map(checkpoint_llm_error)
    }

    fn operation<T>(
        &self,
        operation: impl FnOnce(&mut CheckpointResumeController) -> CheckpointResult<T>,
    ) -> Option<CheckpointResult<T>> {
        self.controller.as_ref().map(|controller| {
            let mut controller = lock_controller(controller)?;
            operation(&mut controller)
        })
    }

    fn failure(
        &self,
        error: CheckpointError,
        messages: &[Message],
        cycles: &[CycleRecord],
        shared_state: &BTreeMap<String, Value>,
    ) -> AgentResult {
        *self
            .pending_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error);
        checkpoint_failed_result(messages, cycles, shared_state, &self.model_call_ledger)
    }
}

fn checkpoint_failed_result(
    messages: &[Message],
    cycles: &[CycleRecord],
    shared_state: &BTreeMap<String, Value>,
    model_call_ledger: &ModelCallLedger,
) -> AgentResult {
    failed_agent_result(
        messages.to_vec(),
        cycles.to_vec(),
        shared_state.clone(),
        "checkpoint runtime failed".to_string(),
        model_call_ledger.usage(),
    )
}

fn lock_controller(
    controller: &CheckpointController,
) -> CheckpointResult<MutexGuard<'_, CheckpointResumeController>> {
    controller.lock().map_err(|_| {
        CheckpointError::new(
            "checkpoint_store_lock_poisoned",
            "checkpoint controller lock poisoned",
        )
    })
}

fn checkpoint_llm_error(error: CheckpointError) -> LlmError {
    LlmError::Request(format!("{}: {}", error.code(), error.message()))
}
