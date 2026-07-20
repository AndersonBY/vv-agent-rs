use std::sync::Arc;

use crate::budget::{BudgetUsageSnapshot, RunBudgetLimits};
use crate::runtime::state::{Checkpoint, StateStore};
use crate::runtime::token_usage::summarize_task_token_usage;
use crate::runtime::CancellationToken;
use crate::types::{
    last_assistant_output, AgentResult, AgentStatus, AgentTask, CompletionReason, Message, Metadata,
};

use super::super::{failed_backend_result, RuntimeRecipe};
use super::backend::DistributedBackend;
use super::checkpoint::{checkpoint_snapshot, load_checkpoint, terminal_checkpoint};
use super::contract::{now_unix_ms, DistributedRunEnvelope};
use super::dispatch::{CycleDispatchResult, CycleDispatcher};

pub(super) struct DistributedRunContext<'a> {
    pub task: &'a AgentTask,
    pub recipe: &'a RuntimeRecipe,
    pub state_store: &'a Arc<dyn StateStore>,
    pub cycle_dispatcher: &'a Arc<dyn CycleDispatcher>,
    pub cancellation_token: Option<&'a CancellationToken>,
    pub max_cycles: u32,
    pub budget_limits: Option<RunBudgetLimits>,
    pub initial_budget_usage: Option<BudgetUsageSnapshot>,
}

impl DistributedBackend {
    pub(super) fn execute_distributed(
        &self,
        initial_messages: Vec<Message>,
        shared_state: Metadata,
        context: DistributedRunContext<'_>,
    ) -> AgentResult {
        let checkpoint = Checkpoint {
            task_id: context.task.task_id.clone(),
            cycle_index: 0,
            status: AgentStatus::Running,
            messages: initial_messages.clone(),
            cycles: Vec::new(),
            shared_state: shared_state.clone(),
            revision: 0,
            claim_token: None,
            claimed_cycle: None,
            lease_expires_at_ms: None,
            terminal_result: None,
            budget_usage: context.initial_budget_usage.clone(),
        };
        match context.state_store.create_checkpoint(checkpoint.clone()) {
            Ok(true) => self.distributed_loop(&context, checkpoint),
            Ok(false) => {
                let operation = format!(
                    "checkpoint create conflict for task {}",
                    context.task.task_id
                );
                match load_checkpoint(context.state_store, &context.task.task_id, &operation) {
                    Ok(Some(existing)) => self.resume_existing(&context, existing),
                    Ok(None) => Self::coordination_failure(
                        &checkpoint,
                        format!("{operation}: checkpoint disappeared before it could be recovered"),
                    ),
                    Err(error) => Self::coordination_failure(&checkpoint, error),
                }
            }
            Err(error) => failed_backend_result(
                initial_messages,
                Vec::new(),
                shared_state,
                format!("Failed to save initial checkpoint: {error}"),
            ),
        }
    }

    fn resume_existing(
        &self,
        context: &DistributedRunContext<'_>,
        checkpoint: Checkpoint,
    ) -> AgentResult {
        if checkpoint.terminal_result.is_some() {
            return self.acknowledge_terminal(
                context,
                &checkpoint,
                "checkpoint create conflict terminal replay",
            );
        }
        if checkpoint.claim_token.is_some() {
            let now_ms = match now_unix_ms() {
                Ok(now_ms) => now_ms,
                Err(error) => {
                    return Self::coordination_failure(
                        &checkpoint,
                        format!(
                            "checkpoint create conflict for task {}: failed to inspect claim lease: {error}",
                            context.task.task_id
                        ),
                    )
                }
            };
            if checkpoint.lease_expires_at_ms.unwrap_or(u64::MAX) > now_ms {
                return Self::coordination_failure(
                    &checkpoint,
                    format!(
                        "checkpoint create conflict for task {}: work is already claimed and in progress",
                        context.task.task_id
                    ),
                );
            }
        }
        if checkpoint.status != AgentStatus::Running {
            return Self::coordination_failure(
                &checkpoint,
                format!(
                    "checkpoint create conflict for task {}: expected running status, found {:?}",
                    context.task.task_id, checkpoint.status
                ),
            );
        }
        if checkpoint.cycle_index > context.max_cycles {
            return Self::coordination_failure(
                &checkpoint,
                format!(
                    "checkpoint create conflict for task {}: durable cycle {} exceeds scheduler max_cycles {}",
                    context.task.task_id, checkpoint.cycle_index, context.max_cycles
                ),
            );
        }
        self.distributed_loop(context, checkpoint)
    }

    fn distributed_loop(
        &self,
        context: &DistributedRunContext<'_>,
        mut observed: Checkpoint,
    ) -> AgentResult {
        let Some(cycle_index_start) = observed.cycle_index.checked_add(1) else {
            return Self::coordination_failure(
                &observed,
                format!(
                    "cannot resume task {} because cycle_index overflowed",
                    context.task.task_id
                ),
            );
        };

        for cycle_index in cycle_index_start..=context.max_cycles {
            if context
                .cancellation_token
                .is_some_and(CancellationToken::is_cancelled)
            {
                return self.finalize_cancellation(
                    context,
                    &observed,
                    Self::cancellation_reason(context.cancellation_token),
                );
            }

            let now_ms = match now_unix_ms() {
                Ok(now_ms) => now_ms,
                Err(error) => {
                    return self.finalize_scheduler_failure(
                        context,
                        &observed,
                        format!("Distributed cycle {cycle_index} failed: {error}"),
                    )
                }
            };
            let timeout_ms = match u64::try_from(self.dispatch_timeout.as_millis()) {
                Ok(timeout_ms) => timeout_ms,
                Err(_) => {
                    return self.finalize_scheduler_failure(
                        context,
                        &observed,
                        format!(
                            "Distributed cycle {cycle_index} failed: dispatch timeout exceeds u64 milliseconds"
                        ),
                    )
                }
            };
            let Some(deadline_unix_ms) = now_ms.checked_add(timeout_ms) else {
                return self.finalize_scheduler_failure(
                    context,
                    &observed,
                    format!("Distributed cycle {cycle_index} failed: dispatch deadline overflow"),
                );
            };
            let envelope = match DistributedRunEnvelope::for_cycle(
                context.task.clone(),
                context.recipe.clone(),
                cycle_index,
                self.cycle_name.clone(),
                None,
                Some(deadline_unix_ms),
                self.lease_duration_ms,
                context.budget_limits.clone(),
            ) {
                Ok(envelope) => envelope,
                Err(error) => {
                    return self.finalize_scheduler_failure(
                        context,
                        &observed,
                        format!("Distributed cycle {cycle_index} failed: {error}"),
                    )
                }
            };

            match context
                .cycle_dispatcher
                .dispatch_envelope_with_cancellation(&envelope, context.cancellation_token)
            {
                Ok(dispatch_result) if dispatch_result.finished => {
                    return self.handle_finished(context, &observed, cycle_index, dispatch_result);
                }
                Ok(_) => match self.verify_unfinished(context, &observed, cycle_index) {
                    Ok(checkpoint) => observed = checkpoint,
                    Err(result) => return result,
                },
                Err(_error)
                    if context
                        .cancellation_token
                        .is_some_and(CancellationToken::is_cancelled) =>
                {
                    return self.finalize_cancellation(
                        context,
                        &observed,
                        Self::cancellation_reason(context.cancellation_token),
                    );
                }
                Err(error) => {
                    return self.handle_dispatch_error(context, &observed, cycle_index, error);
                }
            }
        }

        self.finalize_max_cycles(context, &observed)
    }

    #[allow(clippy::result_large_err)]
    fn verify_unfinished(
        &self,
        context: &DistributedRunContext<'_>,
        observed: &Checkpoint,
        cycle_index: u32,
    ) -> Result<Checkpoint, AgentResult> {
        let operation = format!("Distributed cycle {cycle_index} unfinished verification");
        let checkpoint =
            match load_checkpoint(context.state_store, &context.task.task_id, &operation) {
                Ok(Some(checkpoint)) => checkpoint,
                Ok(None) => {
                    return Err(Self::coordination_failure(
                        observed,
                        format!("{operation}: checkpoint disappeared before progress was verified"),
                    ))
                }
                Err(error) => return Err(Self::coordination_failure(observed, error)),
            };

        if checkpoint.terminal_result.is_some() {
            return Err(self.acknowledge_terminal(
                context,
                &checkpoint,
                "unfinished dispatch observed durable terminal",
            ));
        }
        if checkpoint.claim_token.is_some() {
            return Err(Self::coordination_failure(
                &checkpoint,
                format!("{operation}: worker claim is still active; cycle outcome is uncertain"),
            ));
        }
        if checkpoint.status != AgentStatus::Running {
            return Err(Self::coordination_failure(
                &checkpoint,
                format!(
                    "{operation}: expected running status, found {:?}",
                    checkpoint.status
                ),
            ));
        }
        if checkpoint.cycle_index != cycle_index {
            return Err(Self::coordination_failure(
                &checkpoint,
                format!(
                    "{operation}: expected durable cycle_index {cycle_index}, found {}",
                    checkpoint.cycle_index
                ),
            ));
        }
        Ok(checkpoint)
    }

    fn handle_finished(
        &self,
        context: &DistributedRunContext<'_>,
        observed: &Checkpoint,
        cycle_index: u32,
        dispatch_result: CycleDispatchResult,
    ) -> AgentResult {
        let Some(payload_result) = dispatch_result.result else {
            return Self::coordination_failure(
                observed,
                format!("Distributed cycle {cycle_index} finished without result payload"),
            );
        };
        let operation = format!("Distributed cycle {cycle_index} terminal verification");
        let checkpoint =
            match load_checkpoint(context.state_store, &context.task.task_id, &operation) {
                Ok(Some(checkpoint)) => checkpoint,
                Ok(None) => {
                    return Self::coordination_failure_from_result(
                        &payload_result,
                        format!("{operation}: checkpoint disappeared before terminal verification"),
                    )
                }
                Err(error) => {
                    return Self::coordination_failure_from_result(&payload_result, error)
                }
            };

        if let Some(expected_revision) = dispatch_result.checkpoint_revision {
            if checkpoint.revision != expected_revision {
                return Self::coordination_failure(
                    &checkpoint,
                    format!(
                        "{operation}: expected revision {expected_revision}, found {}",
                        checkpoint.revision
                    ),
                );
            }
            let Some(durable_result) = checkpoint.terminal_result.as_ref() else {
                return Self::coordination_failure(
                    &checkpoint,
                    format!("{operation}: revision {expected_revision} is not a durable terminal"),
                );
            };
            if durable_result != &payload_result {
                return Self::coordination_failure(
                    &checkpoint,
                    format!(
                        "{operation}: dispatch result does not match the durable terminal result"
                    ),
                );
            }
            return self.acknowledge_terminal(context, &checkpoint, &operation);
        }

        if let Some(durable_result) = checkpoint.terminal_result.as_ref() {
            if durable_result != &payload_result {
                return Self::coordination_failure(
                    &checkpoint,
                    format!(
                        "{operation}: compatibility result conflicts with the durable terminal result"
                    ),
                );
            }
            return self.acknowledge_terminal(context, &checkpoint, &operation);
        }
        let mut checkpoint = checkpoint;
        checkpoint.cycle_index = cycle_index;
        self.finalize_loaded_and_ack(context, &checkpoint, payload_result, &operation)
    }

    fn handle_dispatch_error(
        &self,
        context: &DistributedRunContext<'_>,
        observed: &Checkpoint,
        cycle_index: u32,
        dispatch_error: String,
    ) -> AgentResult {
        let error = format!("Distributed cycle {cycle_index} failed: {dispatch_error}");
        let operation = error.clone();
        self.finalize_latest_and_ack(context, observed, &operation, move |checkpoint| {
            let (messages, cycles, shared_state) = checkpoint_snapshot(checkpoint);
            Ok(failed_backend_result(messages, cycles, shared_state, error))
        })
    }

    fn finalize_cancellation(
        &self,
        context: &DistributedRunContext<'_>,
        observed: &Checkpoint,
        reason: String,
    ) -> AgentResult {
        self.finalize_latest_and_ack(
            context,
            observed,
            "distributed cancellation",
            move |checkpoint| {
                let (messages, cycles, shared_state) = checkpoint_snapshot(checkpoint);
                let mut result = failed_backend_result(messages, cycles, shared_state, reason);
                result.completion_reason = Some(CompletionReason::Cancelled);
                Ok(result)
            },
        )
    }

    fn finalize_max_cycles(
        &self,
        context: &DistributedRunContext<'_>,
        observed: &Checkpoint,
    ) -> AgentResult {
        self.finalize_latest_and_ack(
            context,
            observed,
            "distributed max-cycles finalization",
            |checkpoint| {
                if checkpoint.cycle_index != context.max_cycles {
                    return Err(format!(
                        "distributed max-cycles finalization: expected durable cycle_index {}, found {}",
                        context.max_cycles, checkpoint.cycle_index
                    ));
                }
                let (messages, cycles, shared_state) = checkpoint_snapshot(checkpoint);
                let token_usage = summarize_task_token_usage(&cycles);
                let partial_output = last_assistant_output(&cycles);
                Ok(AgentResult {
                    status: AgentStatus::MaxCycles,
                    messages,
                    cycles,
                    completion_reason: Some(CompletionReason::MaxCycles),
                    completion_tool_name: None,
                    partial_output,
                    budget_usage: None,
                    budget_exhaustion: None,
                    checkpoint_key: None,
                    resume_observation: None,
                    final_answer: Some(
                        "Reached max cycles without finish signal.".to_string(),
                    ),
                    wait_reason: None,
                    error: None,
                    error_code: None,
                    shared_state,
                    token_usage,
                })
            },
        )
    }

    fn finalize_scheduler_failure(
        &self,
        context: &DistributedRunContext<'_>,
        observed: &Checkpoint,
        error: String,
    ) -> AgentResult {
        let operation = error.clone();
        self.finalize_latest_and_ack(context, observed, &operation, move |checkpoint| {
            let (messages, cycles, shared_state) = checkpoint_snapshot(checkpoint);
            Ok(failed_backend_result(messages, cycles, shared_state, error))
        })
    }

    fn finalize_latest_and_ack<F>(
        &self,
        context: &DistributedRunContext<'_>,
        observed: &Checkpoint,
        operation: &str,
        build_result: F,
    ) -> AgentResult
    where
        F: FnOnce(&Checkpoint) -> Result<AgentResult, String>,
    {
        let checkpoint =
            match load_checkpoint(context.state_store, &context.task.task_id, operation) {
                Ok(Some(checkpoint)) => checkpoint,
                Ok(None) => {
                    return Self::coordination_failure(
                        observed,
                        format!("{operation}: checkpoint no longer exists"),
                    )
                }
                Err(error) => return Self::coordination_failure(observed, error),
            };

        if checkpoint.terminal_result.is_some() {
            return self.acknowledge_terminal(context, &checkpoint, operation);
        }
        if checkpoint.claim_token.is_some() {
            return Self::coordination_failure(
                &checkpoint,
                format!(
                    "{operation}: checkpoint is claimed by a worker; outcome is uncertain and was not overwritten"
                ),
            );
        }
        if checkpoint.status != AgentStatus::Running {
            return Self::coordination_failure(
                &checkpoint,
                format!(
                    "{operation}: expected running status, found {:?}",
                    checkpoint.status
                ),
            );
        }
        let result = match build_result(&checkpoint) {
            Ok(result) => result,
            Err(error) => return Self::coordination_failure(&checkpoint, error),
        };
        self.finalize_loaded_and_ack(context, &checkpoint, result, operation)
    }

    fn finalize_loaded_and_ack(
        &self,
        context: &DistributedRunContext<'_>,
        checkpoint: &Checkpoint,
        result: AgentResult,
        operation: &str,
    ) -> AgentResult {
        if checkpoint.terminal_result.is_some() {
            return Self::coordination_failure(
                checkpoint,
                format!("{operation}: refusing to overwrite an existing terminal checkpoint"),
            );
        }
        if checkpoint.claim_token.is_some() {
            return Self::coordination_failure(
                checkpoint,
                format!(
                    "{operation}: checkpoint is claimed by a worker; outcome is uncertain and was not overwritten"
                ),
            );
        }
        let Some(terminal_revision) = checkpoint.revision.checked_add(1) else {
            return Self::coordination_failure(
                checkpoint,
                format!("{operation}: checkpoint revision overflow"),
            );
        };
        let mut terminal = terminal_checkpoint(checkpoint, &result);
        match context
            .state_store
            .finalize_checkpoint(terminal.clone(), checkpoint.revision)
        {
            Ok(true) => {
                terminal.revision = terminal_revision;
                self.acknowledge_terminal(context, &terminal, operation)
            }
            Ok(false) => Self::coordination_failure(
                checkpoint,
                format!("{operation}: terminal finalize CAS was rejected"),
            ),
            Err(error) => Self::coordination_failure(
                checkpoint,
                format!("{operation}: terminal finalize failed: {error}"),
            ),
        }
    }

    fn acknowledge_terminal(
        &self,
        context: &DistributedRunContext<'_>,
        checkpoint: &Checkpoint,
        operation: &str,
    ) -> AgentResult {
        let Some(result) = checkpoint.terminal_result.clone() else {
            return Self::coordination_failure(
                checkpoint,
                format!("{operation}: checkpoint is not terminal"),
            );
        };
        if !Self::terminal_checkpoint_is_consistent(checkpoint) {
            return Self::coordination_failure(
                checkpoint,
                format!("{operation}: durable checkpoint fields do not match its terminal result"),
            );
        }
        match context
            .state_store
            .acknowledge_terminal(&context.task.task_id, checkpoint.revision)
        {
            Ok(true) => result,
            Ok(false) => {
                let reconciliation = format!("{operation} acknowledgement reconciliation");
                match load_checkpoint(
                    context.state_store,
                    &context.task.task_id,
                    &reconciliation,
                ) {
                    Ok(None) => result,
                    Ok(Some(current)) => Self::coordination_failure(
                        &current,
                        format!(
                            "{operation}: terminal acknowledgement CAS was rejected and the checkpoint still exists"
                        ),
                    ),
                    Err(error) => Self::coordination_failure_from_result(&result, error),
                }
            }
            Err(error) => Self::coordination_failure_from_result(
                &result,
                format!("{operation}: terminal acknowledgement failed: {error}"),
            ),
        }
    }

    fn coordination_failure(checkpoint: &Checkpoint, error: String) -> AgentResult {
        let (messages, cycles, shared_state) = checkpoint_snapshot(checkpoint);
        failed_backend_result(
            messages,
            cycles,
            shared_state,
            format!("Distributed coordination failure: {error}"),
        )
    }

    fn coordination_failure_from_result(result: &AgentResult, error: String) -> AgentResult {
        failed_backend_result(
            result.messages.clone(),
            result.cycles.clone(),
            result.shared_state.clone(),
            format!("Distributed coordination failure: {error}"),
        )
    }

    fn terminal_checkpoint_is_consistent(checkpoint: &Checkpoint) -> bool {
        checkpoint.terminal_result.as_ref().is_some_and(|result| {
            !matches!(result.status, AgentStatus::Pending | AgentStatus::Running)
                && checkpoint.status == result.status
                && checkpoint.messages == result.messages
                && checkpoint.cycles == result.cycles
                && checkpoint.shared_state == result.shared_state
        })
    }

    fn cancellation_reason(cancellation_token: Option<&CancellationToken>) -> String {
        cancellation_token
            .and_then(CancellationToken::reason)
            .unwrap_or_else(|| "Operation was cancelled".to_string())
    }
}
