use std::sync::MutexGuard;
use std::time::Duration;

use crate::budget::RunBudgetLimits;
use crate::checkpoint::{CheckpointError, CheckpointStatus, ClaimMode, ResumePolicy};
use crate::runtime::backends::RuntimeRecipe;
use crate::runtime::checkpoint_resume::{CheckpointController, CheckpointResumeController};
use crate::runtime::token_usage::summarize_task_token_usage;
use crate::runtime::CancellationToken;
use crate::types::{last_assistant_output, AgentResult, AgentStatus, AgentTask, CompletionReason};

use super::backend::DistributedBackend;
use super::contract::{now_unix_ms, DistributedCheckpointConfig, DistributedRunEnvelope};
use super::dispatch::CycleDispatchResult;

impl DistributedBackend {
    pub(super) fn execute_distributed_v2(
        &self,
        task: &AgentTask,
        cycle_index_start: u32,
        cycle_count: u32,
        budget_limits: Option<RunBudgetLimits>,
        cancellation_token: Option<&CancellationToken>,
        checkpoint_controller: CheckpointController,
    ) -> AgentResult {
        let Some(recipe) = self.runtime_recipe.as_ref() else {
            return controller_failure(
                &checkpoint_controller,
                "distributed v2 requires a runtime recipe".to_string(),
            );
        };
        let Some(dispatcher) = self.cycle_dispatcher.as_ref() else {
            return controller_failure(
                &checkpoint_controller,
                "distributed v2 requires a cycle dispatcher".to_string(),
            );
        };
        if cycle_count == 0 {
            return max_cycles_result(&checkpoint_controller);
        }
        let cycle_index_end = cycle_index_start
            .saturating_add(cycle_count.saturating_sub(1))
            .min(task.max_cycles);
        let first_claim_mode = match lock_controller(&checkpoint_controller) {
            Ok(controller) => controller.next_claim_mode(),
            Err(error) => return checkpoint_error_result(&checkpoint_controller, error),
        };

        for cycle_index in cycle_index_start..=cycle_index_end {
            if let Some(reason) = cancellation_reason(cancellation_token) {
                return cancellation_result(&checkpoint_controller, reason);
            }
            let claim_mode = if cycle_index == cycle_index_start {
                first_claim_mode
            } else {
                ClaimMode::Continue
            };
            let dispatch = match self.dispatch_checkpoint_v2_cycle(
                task,
                recipe,
                dispatcher.as_ref(),
                cycle_index,
                claim_mode,
                budget_limits.clone(),
                cancellation_token,
                &checkpoint_controller,
            ) {
                Ok(result) => result,
                Err(error) => return controller_failure(&checkpoint_controller, error.to_string()),
            };

            if dispatch.finished {
                return match handle_terminal_dispatch(
                    dispatch,
                    cycle_index,
                    self.lease_duration_ms,
                    &checkpoint_controller,
                ) {
                    Ok(result) => result,
                    Err(error) => checkpoint_error_result(&checkpoint_controller, error),
                };
            }
            if let Err(error) =
                verify_committed_cycle(&dispatch, cycle_index, &checkpoint_controller)
            {
                return checkpoint_error_result(&checkpoint_controller, error);
            }
        }

        max_cycles_result(&checkpoint_controller)
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_checkpoint_v2_cycle(
        &self,
        task: &AgentTask,
        recipe: &RuntimeRecipe,
        dispatcher: &dyn super::CycleDispatcher,
        cycle_index: u32,
        mut claim_mode: ClaimMode,
        budget_limits: Option<RunBudgetLimits>,
        cancellation_token: Option<&CancellationToken>,
        checkpoint_controller: &CheckpointController,
    ) -> Result<CycleDispatchResult, CheckpointError> {
        let mut effective_recipe = recipe.clone();
        let metadata_denials = crate::runtime::tool_planner::projected_metadata_denials(task)
            .map_err(|error| checkpoint_error("checkpoint_dispatch_failed", error))?;
        effective_recipe
            .capabilities
            .tool_policy
            .set_metadata_denials(&metadata_denials);
        let timeout_ms = u64::try_from(self.dispatch_timeout.as_millis()).map_err(|_| {
            checkpoint_error(
                "checkpoint_dispatch_failed",
                "distributed dispatch timeout exceeds u64 milliseconds",
            )
        })?;
        let deadline_unix_ms = now_unix_ms()
            .map_err(|error| checkpoint_error("checkpoint_dispatch_failed", error))?
            .checked_add(timeout_ms)
            .ok_or_else(|| {
                checkpoint_error(
                    "checkpoint_dispatch_failed",
                    "distributed dispatch deadline overflow",
                )
            })?;
        let mut last_error = None;

        loop {
            if let Some(reason) = cancellation_reason(cancellation_token) {
                return Err(checkpoint_error("checkpoint_dispatch_cancelled", reason));
            }
            let now_ms = now_unix_ms()
                .map_err(|error| checkpoint_error("checkpoint_dispatch_failed", error))?;
            if now_ms >= deadline_unix_ms {
                let detail = last_error
                    .as_deref()
                    .map(|error| format!(": {error}"))
                    .unwrap_or_default();
                return Err(checkpoint_error(
                    "checkpoint_dispatch_failed",
                    format!(
                        "distributed cycle {cycle_index} exhausted its dispatch deadline{detail}"
                    ),
                ));
            }

            let (checkpoint, checkpoint_config) = {
                let mut controller = lock_controller(checkpoint_controller)?;
                let checkpoint = controller.refresh_authoritative()?;
                let config = distributed_checkpoint_config(&controller)?;
                (checkpoint, config)
            };
            if let Some(terminal_result) = checkpoint.terminal_result.as_ref() {
                let result = AgentResult::from_dict(terminal_result).map_err(|error| {
                    checkpoint_error("checkpoint_terminal_result_invalid", error)
                })?;
                return Ok(CycleDispatchResult::terminal_replay(
                    result,
                    checkpoint.revision,
                ));
            }
            if checkpoint.cycle_index >= u64::from(cycle_index) {
                if checkpoint.cycle_index != u64::from(cycle_index)
                    || checkpoint.claim_token.is_some()
                {
                    return Err(checkpoint_error(
                        "checkpoint_cycle_conflict",
                        "distributed checkpoint advanced beyond the dispatched cycle",
                    ));
                }
                return Ok(CycleDispatchResult::committed(
                    checkpoint.cycle_index,
                    checkpoint.revision,
                ));
            }
            if let Some(lease_expires_at_ms) = checkpoint.lease_expires_at_ms {
                if checkpoint.claim_token.is_some() && lease_expires_at_ms > now_ms {
                    let sleep_ms = lease_expires_at_ms.saturating_sub(now_ms).clamp(1, 100);
                    std::thread::sleep(Duration::from_millis(sleep_ms));
                    continue;
                }
                if checkpoint.claim_token.is_some() {
                    claim_mode = ClaimMode::Recovery;
                }
            }
            if checkpoint.status == CheckpointStatus::ReconciliationRequired || last_error.is_some()
            {
                claim_mode = ClaimMode::Recovery;
            }

            validate_checkpoint_store_ref(
                checkpoint_controller,
                effective_recipe.capabilities.checkpoint_store_ref.as_ref(),
            )?;
            let envelope = DistributedRunEnvelope::for_checkpoint_cycle(
                task.clone(),
                effective_recipe.clone(),
                cycle_index,
                self.cycle_name.clone(),
                Some(checkpoint.root_run_id.clone()),
                Some(deadline_unix_ms),
                self.lease_duration_ms,
                budget_limits.clone(),
                checkpoint.root_run_id.clone(),
                checkpoint.trace_id.clone(),
                checkpoint.run_definition_digest.clone(),
                claim_mode,
                checkpoint.resume_attempt,
                checkpoint_config,
            )
            .map_err(|error| checkpoint_error("checkpoint_dispatch_failed", error))?;

            match dispatcher.dispatch_envelope_with_cancellation(&envelope, cancellation_token) {
                Ok(result)
                    if !result.finished
                        && result.result.is_none()
                        && result.checkpoint_revision.is_none()
                        && result.committed_cycle.is_none()
                        && !result.terminal_candidate
                        && !result.terminal_replay =>
                {
                    last_error = Some(
                        "distributed worker reported pending delivery without committed state"
                            .to_string(),
                    );
                    claim_mode = ClaimMode::Recovery;
                    std::thread::sleep(Duration::from_millis(1));
                }
                Ok(result) => return Ok(result),
                Err(error) if retryable_dispatch_error(&error) => {
                    eprintln!(
                        "warning: distributed checkpoint cycle {cycle_index} will retry after dispatch error: {error}"
                    );
                    last_error = Some(error);
                    claim_mode = ClaimMode::Recovery;
                }
                Err(error) => {
                    return Err(checkpoint_error("checkpoint_dispatch_failed", error));
                }
            }
        }
    }
}

fn verify_committed_cycle(
    dispatch: &CycleDispatchResult,
    cycle_index: u32,
    checkpoint_controller: &CheckpointController,
) -> Result<(), CheckpointError> {
    if dispatch.result.is_some() || dispatch.terminal_candidate || dispatch.terminal_replay {
        return Err(checkpoint_error(
            "checkpoint_store_conflict",
            "distributed unfinished result contains terminal data",
        ));
    }
    let mut controller = lock_controller(checkpoint_controller)?;
    let checkpoint = controller.refresh_authoritative()?;
    if checkpoint.terminal_result.is_some()
        || checkpoint.claim_token.is_some()
        || checkpoint.status != CheckpointStatus::Running
        || checkpoint.cycle_index != u64::from(cycle_index)
    {
        return Err(checkpoint_error(
            "checkpoint_store_conflict",
            "distributed worker progress does not match the durable checkpoint",
        ));
    }
    if dispatch
        .checkpoint_revision
        .is_some_and(|revision| revision != checkpoint.revision)
        || dispatch
            .committed_cycle
            .is_some_and(|committed| committed != checkpoint.cycle_index)
    {
        return Err(checkpoint_error(
            "checkpoint_store_conflict",
            "distributed worker progress revision or cycle does not match the checkpoint",
        ));
    }
    controller.set_next_claim_mode(ClaimMode::Continue);
    Ok(())
}

fn handle_terminal_dispatch(
    mut dispatch: CycleDispatchResult,
    cycle_index: u32,
    lease_duration_ms: u64,
    checkpoint_controller: &CheckpointController,
) -> Result<AgentResult, CheckpointError> {
    let result = dispatch.result.take().ok_or_else(|| {
        checkpoint_error(
            "checkpoint_store_conflict",
            "distributed terminal result is missing its payload",
        )
    })?;
    let mut controller = lock_controller(checkpoint_controller)?;
    let checkpoint = controller.refresh_authoritative()?;
    if dispatch.terminal_replay {
        if checkpoint.terminal_result.is_none()
            || dispatch.checkpoint_revision != Some(checkpoint.revision)
        {
            return Err(checkpoint_error(
                "checkpoint_store_conflict",
                "distributed terminal replay does not match the durable checkpoint",
            ));
        }
        return Ok(result);
    }
    if !dispatch.terminal_candidate {
        return Err(checkpoint_error(
            "checkpoint_store_conflict",
            "distributed v2 worker returned a terminal without candidate semantics",
        ));
    }
    if dispatch.checkpoint_revision != Some(checkpoint.revision) {
        return Err(checkpoint_error(
            "checkpoint_store_conflict",
            "distributed terminal candidate revision does not match the checkpoint",
        ));
    }
    if result.status == AgentStatus::ReconciliationRequired {
        if checkpoint.status != CheckpointStatus::ReconciliationRequired
            || checkpoint.claim_token.is_some()
        {
            return Err(checkpoint_error(
                "checkpoint_store_conflict",
                "distributed reconciliation candidate does not match durable state",
            ));
        }
        return Ok(result);
    }
    if !matches!(
        result.status,
        AgentStatus::WaitUser
            | AgentStatus::Completed
            | AgentStatus::Failed
            | AgentStatus::MaxCycles
    ) || checkpoint.terminal_result.is_some()
    {
        return Err(checkpoint_error(
            "checkpoint_store_conflict",
            "distributed terminal candidate is not eligible for finalization",
        ));
    }
    if checkpoint.claimed_cycle != Some(u64::from(cycle_index)) {
        return Err(checkpoint_error(
            "checkpoint_cycle_conflict",
            "distributed terminal candidate belongs to a different claimed cycle",
        ));
    }
    if result
        .cycles
        .last()
        .is_some_and(|cycle| cycle.index != cycle_index)
    {
        return Err(checkpoint_error(
            "checkpoint_cycle_conflict",
            "distributed terminal candidate does not contain the dispatched cycle",
        ));
    }
    let claim_token = checkpoint.claim_token.as_deref().ok_or_else(|| {
        checkpoint_error(
            "checkpoint_claim_active",
            "distributed terminal candidate no longer has an active claim",
        )
    })?;
    controller.adopt_claim_for_terminal_finalize(claim_token, lease_duration_ms)?;
    Ok(result)
}

fn distributed_checkpoint_config(
    controller: &CheckpointResumeController,
) -> Result<DistributedCheckpointConfig, CheckpointError> {
    let config = controller.checkpoint_config();
    let key = controller.checkpoint_key()?.to_string();
    let distributed = DistributedCheckpointConfig {
        key,
        resume_policy: ResumePolicy::RequireExisting,
        ambiguous_model_policy: config.ambiguous_model_policy,
        ambiguous_tool_policy: config.ambiguous_tool_policy,
        required_extension_namespaces: config.required_extension_namespaces.clone(),
        max_extension_state_bytes: config.max_extension_state_bytes,
        credential_slots: config.credential_slots.clone(),
    };
    distributed
        .validate()
        .map_err(|error| checkpoint_error("checkpoint_config_invalid", error))?;
    Ok(distributed)
}

fn validate_checkpoint_store_ref(
    checkpoint_controller: &CheckpointController,
    recipe_ref: Option<&super::CapabilityRef>,
) -> Result<(), CheckpointError> {
    let controller = lock_controller(checkpoint_controller)?;
    let config = controller.checkpoint_config();
    let expected = config
        .store_ref
        .as_ref()
        .or_else(|| config.capability_ref("checkpoint_store"));
    if let Some(expected) = expected {
        if recipe_ref != Some(expected) {
            return Err(checkpoint_error(
                "checkpoint_definition_mismatch",
                "distributed checkpoint store capability does not match CheckpointConfig.store_ref",
            ));
        }
    }
    Ok(())
}

fn max_cycles_result(checkpoint_controller: &CheckpointController) -> AgentResult {
    match authoritative_checkpoint(checkpoint_controller) {
        Ok(checkpoint) => AgentResult {
            status: AgentStatus::MaxCycles,
            completion_reason: Some(CompletionReason::MaxCycles),
            partial_output: last_assistant_output(&checkpoint.cycles),
            token_usage: summarize_task_token_usage(&checkpoint.cycles),
            messages: checkpoint.messages,
            cycles: checkpoint.cycles,
            budget_usage: checkpoint.budget_usage,
            final_answer: Some("Reached max cycles without finish signal.".to_string()),
            shared_state: checkpoint.shared_state,
            ..AgentResult::default()
        },
        Err(error) => checkpoint_error_result(checkpoint_controller, error),
    }
}

fn cancellation_result(
    checkpoint_controller: &CheckpointController,
    reason: String,
) -> AgentResult {
    match authoritative_checkpoint(checkpoint_controller) {
        Ok(checkpoint) => AgentResult {
            status: AgentStatus::Failed,
            completion_reason: Some(CompletionReason::Cancelled),
            partial_output: last_assistant_output(&checkpoint.cycles),
            token_usage: summarize_task_token_usage(&checkpoint.cycles),
            messages: checkpoint.messages,
            cycles: checkpoint.cycles,
            budget_usage: checkpoint.budget_usage,
            error: Some(reason),
            shared_state: checkpoint.shared_state,
            ..AgentResult::default()
        },
        Err(error) => checkpoint_error_result(checkpoint_controller, error),
    }
}

fn controller_failure(checkpoint_controller: &CheckpointController, error: String) -> AgentResult {
    match authoritative_checkpoint(checkpoint_controller) {
        Ok(checkpoint) => AgentResult {
            status: AgentStatus::Failed,
            completion_reason: Some(CompletionReason::Failed),
            partial_output: last_assistant_output(&checkpoint.cycles),
            token_usage: summarize_task_token_usage(&checkpoint.cycles),
            messages: checkpoint.messages,
            cycles: checkpoint.cycles,
            budget_usage: checkpoint.budget_usage,
            error: Some(error),
            shared_state: checkpoint.shared_state,
            ..AgentResult::default()
        },
        Err(checkpoint_error) => checkpoint_error_result(checkpoint_controller, checkpoint_error),
    }
}

fn checkpoint_error_result(
    checkpoint_controller: &CheckpointController,
    error: CheckpointError,
) -> AgentResult {
    let mut result = AgentResult::failed(error.to_string());
    if let Ok(controller) = checkpoint_controller.lock() {
        if let Ok(checkpoint) = controller.checkpoint() {
            result.messages = checkpoint.messages.clone();
            result.cycles = checkpoint.cycles.clone();
            result.shared_state = checkpoint.shared_state.clone();
            result.budget_usage = checkpoint.budget_usage.clone();
            result.partial_output = last_assistant_output(&result.cycles);
            result.token_usage = summarize_task_token_usage(&result.cycles);
        }
    }
    result
}

fn authoritative_checkpoint(
    checkpoint_controller: &CheckpointController,
) -> Result<crate::CheckpointV2, CheckpointError> {
    lock_controller(checkpoint_controller)?.refresh_authoritative()
}

fn lock_controller(
    checkpoint_controller: &CheckpointController,
) -> Result<MutexGuard<'_, CheckpointResumeController>, CheckpointError> {
    checkpoint_controller.lock().map_err(|_| {
        checkpoint_error(
            "checkpoint_store_lock_poisoned",
            "checkpoint controller lock poisoned",
        )
    })
}

fn cancellation_reason(cancellation_token: Option<&CancellationToken>) -> Option<String> {
    cancellation_token
        .and_then(|token| token.check().err())
        .map(|error| error.to_string())
}

fn retryable_dispatch_error(error: &str) -> bool {
    [
        "checkpoint_claim_active",
        "checkpoint_lease_lost",
        "checkpoint_store_conflict",
        "retryable distributed v2 delivery conflict",
    ]
    .iter()
    .any(|candidate| error.contains(candidate))
}

fn checkpoint_error(code: &'static str, message: impl Into<String>) -> CheckpointError {
    CheckpointError::new(code, message)
}
