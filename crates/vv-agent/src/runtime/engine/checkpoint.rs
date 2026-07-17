use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::Value;

use crate::budget::BudgetUsageSnapshot;
use crate::checkpoint::{CheckpointError, CheckpointResult, ToolIdempotency};
use crate::llm::{LlmError, LlmRequest};
use crate::runtime::checkpoint_resume::{
    CheckpointController, CheckpointResumeController, ModelOperationOutcome, ToolOperationPlan,
};
use crate::tools::{BeforeToolDispatch, ToolError, ToolRunOptions};
use crate::types::{AgentResult, CycleRecord, LLMResponse, Message, ToolCall, ToolExecutionResult};

use super::helpers::failed_agent_result;

type PendingCheckpointError = Arc<Mutex<Option<CheckpointError>>>;
type FailureContext<'a> = (
    &'a [Message],
    &'a [CycleRecord],
    &'a BTreeMap<String, Value>,
);

pub(super) enum CheckpointModelCompletion {
    Continue(Box<Result<LLMResponse, LlmError>>),
    Stop(Box<AgentResult>),
}

pub(super) enum CheckpointToolPlan {
    Continue(Option<ToolOperationPlan>),
    Stop(Box<AgentResult>),
}

pub(super) struct CheckpointCoordinator {
    controller: Option<CheckpointController>,
    pending_error: PendingCheckpointError,
}

impl CheckpointCoordinator {
    pub(super) fn new(controller: Option<CheckpointController>) -> Self {
        Self {
            controller,
            pending_error: Arc::new(Mutex::new(None)),
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
        cycle_index: u32,
        operation_slot: &str,
        request: LlmRequest,
        budget_usage: B,
        invoke: F,
        failure_context: FailureContext<'_>,
    ) -> CheckpointModelCompletion
    where
        F: FnOnce(LlmRequest) -> Result<LLMResponse, LlmError>,
        B: FnOnce() -> Option<BudgetUsageSnapshot>,
    {
        let Some(controller) = self.controller.as_ref() else {
            return CheckpointModelCompletion::Continue(Box::new(invoke(request)));
        };
        let invoke_request = request.clone();
        let budget_usage = budget_usage();
        let outcome = lock_controller(controller).and_then(|mut controller| {
            controller.complete_model(cycle_index, operation_slot, &request, budget_usage, || {
                invoke(invoke_request)
            })
        });
        match outcome {
            Ok(ModelOperationOutcome::Response(response)) => {
                CheckpointModelCompletion::Continue(Box::new(Ok(*response)))
            }
            Ok(ModelOperationOutcome::Error(error)) => {
                CheckpointModelCompletion::Continue(Box::new(Err(error)))
            }
            Ok(ModelOperationOutcome::Interrupted(result)) => {
                CheckpointModelCompletion::Stop(result)
            }
            Err(error) => CheckpointModelCompletion::Stop(Box::new(self.failure(
                error,
                failure_context.0,
                failure_context.1,
                failure_context.2,
            ))),
        }
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
            Ok((plan, None)) => CheckpointToolPlan::Continue(Some(plan)),
            Err(error) => CheckpointToolPlan::Stop(Box::new(self.failure(
                error,
                messages,
                cycles,
                shared_state,
            ))),
        }
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
            let outcome = lock_controller(&controller)
                .and_then(|mut controller| controller.tool_started(cycle_index, call));
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
            .then(|| checkpoint_failed_result(messages, cycles, shared_state))
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
        checkpoint_failed_result(messages, cycles, shared_state)
    }
}

fn checkpoint_failed_result(
    messages: &[Message],
    cycles: &[CycleRecord],
    shared_state: &BTreeMap<String, Value>,
) -> AgentResult {
    failed_agent_result(
        messages.to_vec(),
        cycles.to_vec(),
        shared_state.clone(),
        "checkpoint runtime failed".to_string(),
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
