use std::sync::Arc;

use crate::budget::RunBudgetLimits;
use crate::checkpoint::{
    CheckpointStatus, ClaimMode, ControllerCommandVariant, HostInteractionRecoveryEnvelope,
    HostInteractionRequest, HOST_INTERACTION_RECOVERY_SCHEMA, HOST_INTERACTION_REQUEST_SCHEMA,
};
use crate::events::{RunEvent, RunEventPayload};
use crate::runtime::token_usage::summarize_task_token_usage;
use crate::runtime::CheckpointStore;
use crate::types::{last_assistant_output, AgentResult, AgentStatus, AgentTask, CompletionReason};

use super::super::RuntimeRecipe;
use super::backend::DistributedBackend;
use super::capabilities::DistributedCapabilityRegistry;
use super::contract::{now_unix_ms, DistributedCheckpointConfig, DistributedRunEnvelope};
use super::dispatch::CycleDispatchResult;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DistributedRunHandle {
    pub checkpoint_key: String,
    pub run_id: String,
    pub trace_id: String,
}

impl DistributedRunHandle {
    fn from_checkpoint(checkpoint: &crate::runtime::Checkpoint) -> Self {
        Self {
            checkpoint_key: checkpoint.checkpoint_key.clone(),
            run_id: checkpoint.root_run_id.clone(),
            trace_id: checkpoint.trace_id.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum DistributedDeliveryOutcome {
    Worker(Box<CycleDispatchResult>),
    TransportFailure(String),
}

impl DistributedDeliveryOutcome {
    pub fn worker(response: CycleDispatchResult) -> Self {
        Self::Worker(Box::new(response))
    }

    pub fn transport_failure(error: impl Into<String>) -> Result<Self, String> {
        let error = error.into();
        if error.trim().is_empty() {
            return Err("distributed transport error must be a non-empty string".to_string());
        }
        Ok(Self::TransportFailure(error))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DistributedWaitReason {
    ActiveClaim,
    ReconciliationRequired,
    HostInteraction,
    Suspended,
    DeferredPending,
    SupersededDelivery,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DistributedAdvanceDecision {
    Dispatch {
        handle: DistributedRunHandle,
        envelope: DistributedRunEnvelope,
    },
    RetryAt {
        handle: DistributedRunHandle,
        envelope: DistributedRunEnvelope,
        not_before_unix_ms: u64,
    },
    Wait {
        handle: DistributedRunHandle,
        reason: DistributedWaitReason,
    },
    FinalizeRequired {
        handle: DistributedRunHandle,
        checkpoint_revision: u64,
        result: AgentResult,
    },
    TerminalReplay {
        handle: DistributedRunHandle,
        checkpoint_revision: u64,
        result: AgentResult,
    },
}

pub trait CycleEnqueuer: Send + Sync {
    fn enqueue_envelope(
        &self,
        envelope: &DistributedRunEnvelope,
        not_before_unix_ms: Option<u64>,
    ) -> Result<(), String>;
}

type NonblockingComponents<'a> = (
    &'a RuntimeRecipe,
    Arc<dyn CheckpointStore>,
    &'a Arc<dyn CycleEnqueuer>,
);

impl DistributedBackend {
    pub fn nonblocking(
        runtime_recipe: RuntimeRecipe,
        capability_registry: DistributedCapabilityRegistry,
        cycle_enqueuer: Arc<dyn CycleEnqueuer>,
    ) -> Self {
        Self::inline_fallback().with_nonblocking_driver(
            runtime_recipe,
            capability_registry,
            cycle_enqueuer,
        )
    }

    pub fn with_nonblocking_driver(
        mut self,
        runtime_recipe: RuntimeRecipe,
        capability_registry: DistributedCapabilityRegistry,
        cycle_enqueuer: Arc<dyn CycleEnqueuer>,
    ) -> Self {
        self.runtime_recipe = Some(runtime_recipe);
        self.capability_registry = Some(capability_registry);
        self.cycle_enqueuer = Some(cycle_enqueuer);
        self
    }

    pub fn start(
        &self,
        task: AgentTask,
        checkpoint_config: DistributedCheckpointConfig,
        budget_limits: Option<RunBudgetLimits>,
    ) -> Result<DistributedRunHandle, String> {
        self.start_inner(task, checkpoint_config, budget_limits, None)
    }

    pub(crate) fn start_with_controller_store(
        &self,
        task: AgentTask,
        checkpoint_config: DistributedCheckpointConfig,
        budget_limits: Option<RunBudgetLimits>,
        controller_store: Arc<dyn CheckpointStore>,
    ) -> Result<DistributedRunHandle, String> {
        self.start_inner(
            task,
            checkpoint_config,
            budget_limits,
            Some(controller_store),
        )
    }

    fn start_inner(
        &self,
        task: AgentTask,
        checkpoint_config: DistributedCheckpointConfig,
        budget_limits: Option<RunBudgetLimits>,
        controller_store: Option<Arc<dyn CheckpointStore>>,
    ) -> Result<DistributedRunHandle, String> {
        let (recipe, store, enqueuer) = self.nonblocking_components()?;
        checkpoint_config.validate()?;
        if let Some(controller_store) = controller_store.as_deref() {
            if controller_store.store_identity() != store.store_identity() {
                return Err(
                    "checkpoint_store_conflict: distributed start controller and registry stores differ"
                        .to_string(),
                );
            }
        }
        let checkpoint = load_checkpoint_once(store.as_ref(), &checkpoint_config.key)?;
        let handle = DistributedRunHandle::from_checkpoint(&checkpoint);
        if checkpoint.terminal_result.is_some() {
            return Ok(handle);
        }
        if checkpoint.status != CheckpointStatus::Running
            || checkpoint.claim_token.is_some()
            || checkpoint.cycle_index != 0
        {
            return Err(
                "distributed start requires an unclaimed running checkpoint before cycle 1"
                    .to_string(),
            );
        }
        if checkpoint.task_id != task.task_id {
            return Err(
                "distributed start task does not match the authoritative checkpoint".to_string(),
            );
        }
        let envelope = self.envelope_from_checkpoint(
            task,
            recipe.clone(),
            &checkpoint,
            checkpoint_config,
            1,
            ClaimMode::Continue,
            budget_limits,
            None,
        )?;
        enqueuer.enqueue_envelope(&envelope, None)?;
        Ok(handle)
    }

    pub fn advance(
        &self,
        previous_envelope: &DistributedRunEnvelope,
        outcome: DistributedDeliveryOutcome,
    ) -> Result<DistributedAdvanceDecision, String> {
        self.validate_nonblocking_recipe()?;
        previous_envelope.validate()?;
        if previous_envelope
            .recipe
            .capabilities
            .approval_provider_ref
            .is_some()
            || previous_envelope
                .recipe
                .capabilities
                .approval_broker_ref
                .is_some()
        {
            return Err(
                "nonblocking distributed runs do not support brokered approval waits".to_string(),
            );
        }
        if matches!(
            &outcome,
            DistributedDeliveryOutcome::TransportFailure(error) if error.trim().is_empty()
        ) {
            return Err("distributed transport error must be a non-empty string".to_string());
        }
        if let DistributedDeliveryOutcome::Worker(response) = &outcome {
            response.validate()?;
        }
        let registry = self.capability_registry.as_ref().ok_or_else(|| {
            "nonblocking DistributedBackend requires a DistributedCapabilityRegistry".to_string()
        })?;
        let store_reference = previous_envelope
            .recipe
            .capabilities
            .checkpoint_store_ref
            .as_ref()
            .ok_or_else(|| "distributed run requires checkpoint_store_ref".to_string())?;
        let store = registry
            .resolve_checkpoint_store_required(store_reference)
            .map_err(|error| error.to_string())?;

        // This is the one authoritative read performed by an advance invocation.
        let checkpoint =
            load_checkpoint_once(store.as_ref(), &previous_envelope.checkpoint_config.key)?;
        validate_checkpoint_identity(previous_envelope, &checkpoint)?;
        let (checkpoint, recovered_host_response) =
            consume_controller_wakes(store.as_ref(), checkpoint, self.lease_duration_ms)?;
        let handle = DistributedRunHandle::from_checkpoint(&checkpoint);
        let response = match &outcome {
            DistributedDeliveryOutcome::Worker(response) => Some(response.as_ref()),
            DistributedDeliveryOutcome::TransportFailure(_) => None,
        };

        if let Some(terminal_result) = checkpoint.terminal_result.as_ref() {
            let result = AgentResult::from_dict(terminal_result)
                .map_err(|error| format!("invalid durable terminal result: {error}"))?;
            if let Some(CycleDispatchResult::TerminalReplay {
                checkpoint_revision,
                result: observed,
            }) = response
            {
                if *checkpoint_revision != checkpoint.revision || observed != &result {
                    return Err(
                        "distributed terminal replay does not match the durable checkpoint"
                            .to_string(),
                    );
                }
            }
            return Ok(DistributedAdvanceDecision::TerminalReplay {
                handle,
                checkpoint_revision: checkpoint.revision,
                result,
            });
        }

        if matches!(response, Some(CycleDispatchResult::TerminalReplay { .. })) {
            return Err("distributed terminal replay has no matching durable terminal".to_string());
        }

        // A pending worker response is only an observation.  The durable
        // checkpoint remains authoritative for host interaction and
        // suspension; neither state creates a replacement cycle or polls a
        // provider.
        if checkpoint.status == CheckpointStatus::HostInteraction {
            if checkpoint.claim_token.is_some() {
                return Err(
                    "checkpoint_store_conflict: host interaction cannot retain an execution claim"
                        .to_string(),
                );
            }
            return Ok(DistributedAdvanceDecision::Wait {
                handle,
                reason: DistributedWaitReason::HostInteraction,
            });
        }
        if checkpoint.status == CheckpointStatus::Suspended {
            if checkpoint.claim_token.is_some() {
                return Err("checkpoint_store_conflict: suspended checkpoint cannot retain an execution claim".to_string());
            }
            return Ok(DistributedAdvanceDecision::Wait {
                handle,
                reason: DistributedWaitReason::Suspended,
            });
        }

        // Deferred checkpoints are an authoritative barrier. The worker
        // response remains the existing `pending` wire shape; the driver
        // never polls a provider or enqueues a replacement envelope while
        // the barrier still has unresolved handles.
        if checkpoint.status == CheckpointStatus::Deferred {
            if checkpoint.claim_token.is_some() {
                return Err(
                    "checkpoint_store_conflict: distributed deferred checkpoint cannot retain an active claim"
                        .to_string(),
                );
            }
            return Ok(DistributedAdvanceDecision::Wait {
                handle,
                reason: DistributedWaitReason::DeferredPending,
            });
        }

        if let Some(CycleDispatchResult::TerminalCandidate {
            checkpoint_revision,
            result,
        }) = response
        {
            if *checkpoint_revision != checkpoint.revision {
                return Err(
                    "distributed terminal candidate revision does not match the checkpoint"
                        .to_string(),
                );
            }
            if result.status == AgentStatus::ReconciliationRequired {
                if checkpoint.status != CheckpointStatus::ReconciliationRequired
                    || checkpoint.claim_token.is_some()
                {
                    return Err(
                        "distributed reconciliation candidate does not match durable state"
                            .to_string(),
                    );
                }
                return Ok(DistributedAdvanceDecision::Wait {
                    handle,
                    reason: DistributedWaitReason::ReconciliationRequired,
                });
            }
            if checkpoint.claim_token.is_none()
                || checkpoint.claimed_cycle != Some(u64::from(previous_envelope.cycle_index))
            {
                return Err(
                    "distributed terminal candidate does not retain the dispatched cycle claim"
                        .to_string(),
                );
            }
            if result
                .cycles
                .last()
                .is_some_and(|cycle| cycle.index != previous_envelope.cycle_index)
            {
                return Err(
                    "distributed terminal candidate does not contain the dispatched cycle"
                        .to_string(),
                );
            }
            return Ok(DistributedAdvanceDecision::FinalizeRequired {
                handle,
                checkpoint_revision: checkpoint.revision,
                result: result.clone(),
            });
        }

        if checkpoint.status == CheckpointStatus::ReconciliationRequired {
            return Ok(DistributedAdvanceDecision::Wait {
                handle,
                reason: DistributedWaitReason::ReconciliationRequired,
            });
        }

        if let Some(CycleDispatchResult::Committed {
            checkpoint_revision: _,
            committed_cycle,
        }) = response
        {
            if *committed_cycle != u64::from(previous_envelope.cycle_index) {
                return Err(
                    "distributed committed response does not match the dispatched cycle"
                        .to_string(),
                );
            }
            if checkpoint.cycle_index < *committed_cycle {
                return Err("distributed committed response is ahead of the checkpoint".to_string());
            }
        }

        let previous_cycle = u64::from(previous_envelope.cycle_index);
        if !recovered_host_response
            && (checkpoint.cycle_index > previous_cycle
                || checkpoint
                    .claimed_cycle
                    .is_some_and(|claimed_cycle| claimed_cycle > previous_cycle))
        {
            return Ok(DistributedAdvanceDecision::Wait {
                handle,
                reason: DistributedWaitReason::SupersededDelivery,
            });
        }
        if let Some(CycleDispatchResult::Committed {
            checkpoint_revision,
            ..
        }) = response
        {
            if checkpoint.claim_token.is_none() && *checkpoint_revision != checkpoint.revision {
                return Err(
                    "distributed committed response revision does not match the checkpoint"
                        .to_string(),
                );
            }
        }

        let now_ms = now_unix_ms()?;
        let (cycle_index, claim_mode, not_before_unix_ms) = if recovered_host_response {
            let cycle_index = checkpoint.claimed_cycle.ok_or_else(|| {
                "host response recovery did not retain a claimed cycle".to_string()
            })?;
            (cycle_index, ClaimMode::Recovery, None)
        } else if checkpoint.claim_token.is_some() {
            let cycle_index = checkpoint
                .claimed_cycle
                .ok_or_else(|| "distributed checkpoint has a partial claim".to_string())?;
            let not_before = checkpoint
                .lease_expires_at_ms
                .filter(|lease_expires_at_ms| *lease_expires_at_ms > now_ms);
            (cycle_index, ClaimMode::Recovery, not_before)
        } else if checkpoint.cycle_index == previous_cycle {
            (
                checkpoint
                    .cycle_index
                    .checked_add(1)
                    .ok_or_else(|| "distributed cycle index overflow".to_string())?,
                ClaimMode::Continue,
                None,
            )
        } else if checkpoint.cycle_index.checked_add(1) == Some(previous_cycle) {
            (previous_cycle, ClaimMode::Recovery, None)
        } else {
            return Err("distributed delivery is out of order with the checkpoint".to_string());
        };

        if cycle_index > u64::from(previous_envelope.task.max_cycles) {
            let result = AgentResult {
                status: AgentStatus::MaxCycles,
                completion_reason: Some(CompletionReason::MaxCycles),
                partial_output: last_assistant_output(&checkpoint.cycles),
                token_usage: summarize_task_token_usage(&checkpoint.model_calls),
                messages: checkpoint.messages.clone(),
                cycles: checkpoint.cycles.clone(),
                budget_usage: checkpoint.budget_usage.clone(),
                final_answer: Some("Reached max cycles without finish signal.".to_string()),
                shared_state: checkpoint.shared_state.clone(),
                ..AgentResult::default()
            };
            return Ok(DistributedAdvanceDecision::FinalizeRequired {
                handle,
                checkpoint_revision: checkpoint.revision,
                result,
            });
        }

        let cycle_index = u32::try_from(cycle_index)
            .map_err(|_| "distributed cycle index exceeds u32".to_string())?;
        let envelope = self.envelope_from_checkpoint(
            previous_envelope.task.clone(),
            previous_envelope.recipe.clone(),
            &checkpoint,
            previous_envelope.checkpoint_config.clone(),
            cycle_index,
            claim_mode,
            previous_envelope.budget_limits.clone(),
            not_before_unix_ms,
        )?;
        let enqueuer = self
            .cycle_enqueuer
            .as_ref()
            .ok_or_else(|| "nonblocking DistributedBackend requires a CycleEnqueuer".to_string())?;
        if let Some(not_before_unix_ms) = not_before_unix_ms {
            enqueuer.enqueue_envelope(&envelope, Some(not_before_unix_ms))?;
            Ok(DistributedAdvanceDecision::RetryAt {
                handle,
                envelope,
                not_before_unix_ms,
            })
        } else {
            enqueuer.enqueue_envelope(&envelope, None)?;
            Ok(DistributedAdvanceDecision::Dispatch { handle, envelope })
        }
    }

    fn validate_nonblocking_recipe(&self) -> Result<(), String> {
        let recipe = self
            .runtime_recipe
            .as_ref()
            .ok_or_else(|| "nonblocking DistributedBackend requires a RuntimeRecipe".to_string())?;
        recipe.validate()?;
        if recipe.capabilities.approval_provider_ref.is_some()
            || recipe.capabilities.approval_broker_ref.is_some()
        {
            return Err(
                "nonblocking distributed runs do not support brokered approval waits".to_string(),
            );
        }
        Ok(())
    }

    fn nonblocking_components(&self) -> Result<NonblockingComponents<'_>, String> {
        self.validate_nonblocking_recipe()?;
        let recipe = self.runtime_recipe.as_ref().expect("validated above");
        let registry = self.capability_registry.as_ref().ok_or_else(|| {
            "nonblocking DistributedBackend requires a DistributedCapabilityRegistry".to_string()
        })?;
        registry
            .resolve(&recipe.capabilities)
            .map_err(|error| error.to_string())?;
        let reference = recipe
            .capabilities
            .checkpoint_store_ref
            .as_ref()
            .ok_or_else(|| "distributed run requires checkpoint_store_ref".to_string())?;
        let store = registry
            .resolve_checkpoint_store_required(reference)
            .map_err(|error| error.to_string())?;
        let enqueuer = self
            .cycle_enqueuer
            .as_ref()
            .ok_or_else(|| "nonblocking DistributedBackend requires a CycleEnqueuer".to_string())?;
        Ok((recipe, store, enqueuer))
    }

    #[allow(clippy::too_many_arguments)]
    fn envelope_from_checkpoint(
        &self,
        task: AgentTask,
        mut recipe: RuntimeRecipe,
        checkpoint: &crate::runtime::Checkpoint,
        checkpoint_config: DistributedCheckpointConfig,
        cycle_index: u32,
        claim_mode: ClaimMode,
        budget_limits: Option<RunBudgetLimits>,
        not_before_unix_ms: Option<u64>,
    ) -> Result<DistributedRunEnvelope, String> {
        let metadata_denials = crate::runtime::tool_planner::projected_metadata_denials(&task)?;
        recipe
            .capabilities
            .tool_policy
            .set_metadata_denials(&metadata_denials);
        let timeout_ms = u64::try_from(self.dispatch_timeout.as_millis())
            .map_err(|_| "distributed dispatch timeout exceeds u64 milliseconds".to_string())?;
        let deadline_base_ms = now_unix_ms()?.max(not_before_unix_ms.unwrap_or(0));
        let deadline_unix_ms = deadline_base_ms
            .checked_add(timeout_ms)
            .ok_or_else(|| "distributed dispatch deadline overflow".to_string())?;
        DistributedRunEnvelope::for_cycle(
            task,
            recipe,
            cycle_index,
            self.cycle_name.clone(),
            Some(checkpoint.root_run_id.clone()),
            Some(deadline_unix_ms),
            self.lease_duration_ms,
            budget_limits,
            checkpoint.root_run_id.clone(),
            checkpoint.trace_id.clone(),
            checkpoint.run_definition_digest.clone(),
            claim_mode,
            checkpoint.resume_attempt,
            checkpoint_config,
        )
    }
}

fn load_checkpoint_once(
    store: &dyn CheckpointStore,
    checkpoint_key: &str,
) -> Result<crate::runtime::Checkpoint, String> {
    store
        .load_checkpoint(checkpoint_key)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "checkpoint disappeared before distributed driver invocation".to_string())
}

fn consume_controller_wakes(
    store: &dyn CheckpointStore,
    checkpoint: crate::runtime::Checkpoint,
    lease_duration_ms: u64,
) -> Result<(crate::runtime::Checkpoint, bool), String> {
    let retained_recovery_claim = checkpoint
        .claim_token
        .as_deref()
        .is_some_and(|token| token.starts_with("host-recovery:"));
    if checkpoint.status != CheckpointStatus::Running
        || (checkpoint.claim_token.is_some() && !retained_recovery_claim)
    {
        return Ok((checkpoint, false));
    }

    let now_ms = now_unix_ms()?;
    let wakes = store
        .reap_controller_command_wakes(&checkpoint.checkpoint_key, now_ms)
        .map_err(|error| error.to_string())?;
    let mut current = checkpoint;
    let mut consumed_host_response = false;
    for wake in wakes {
        if wake.outbox_state != "pending" {
            return Err("controller wake reaper returned a non-pending wake".to_string());
        }
        if wake.checkpoint_key != current.checkpoint_key
            || wake.handle.checkpoint_key != current.checkpoint_key
            || wake.outbox_action != "recovery_dispatch"
        {
            return Err("controller wake escaped its checkpoint scope".to_string());
        }
        let command = store
            .get_controller_command(&wake.command_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "controller wake command payload is missing".to_string())?;
        if command.handle.checkpoint_key != current.checkpoint_key
            || command.command_id != wake.command_id
            || command.command_digest != wake.command_digest
        {
            return Err("controller wake command identity is inconsistent".to_string());
        }

        let host_recovery = match &command.command {
            ControllerCommandVariant::HostInteractionResponse {
                interaction_id,
                request_digest,
                ..
            } => Some((
                host_request_from_event(&current, interaction_id, request_digest)?,
                command.command_id.clone(),
            )),
            ControllerCommandVariant::Resume => {
                match store
                    .find_resolved_pending_host_interaction(&current.checkpoint_key)
                    .map_err(|error| error.to_string())?
                {
                    Some(record) => {
                        if record.checkpoint_key != current.checkpoint_key
                            || record.state != "resolved_pending"
                        {
                            return Err("resume wake resolved host record is stale".to_string());
                        }
                        let response_command_id = record.command_id.clone().ok_or_else(|| {
                            "resume wake resolved host record has no response command".to_string()
                        })?;
                        let response_command = store
                            .get_controller_command(&response_command_id)
                            .map_err(|error| error.to_string())?
                            .ok_or_else(|| "resume wake response command is missing".to_string())?;
                        if !matches!(
                            response_command.command,
                            ControllerCommandVariant::HostInteractionResponse { .. }
                        ) || response_command.handle.checkpoint_key != current.checkpoint_key
                        {
                            return Err("resume wake response command is stale".to_string());
                        }
                        Some((record.request, response_command_id))
                    }
                    None => {
                        if checkpoint_has_unconsumed_host_interaction(&current)? {
                            return Err(
                                "host interaction recovery record is missing for a resume wake"
                                    .to_string(),
                            );
                        }
                        None
                    }
                }
            }
            _ => {
                return Err("non-waking controller command has a recovery wake".to_string());
            }
        };

        let claim_token = format!("distributed-host-response:{}", wake.command_id);
        let lease_expires_at_ms = now_ms
            .checked_add(lease_duration_ms.max(1_000))
            .ok_or_else(|| "controller wake claim lease overflow".to_string())?;

        if let Some((request, response_command_id)) = host_recovery {
            let record_id = crate::checkpoint::record_id_for(&current.checkpoint_key, &request);
            store
                .reap_host_interaction_record(&record_id, &current.checkpoint_key, now_ms)
                .map_err(|error| error.to_string())?;
            current = load_checkpoint_once(store, &current.checkpoint_key)?;
            let Some(claimed_wake) = store
                .claim_controller_command_wake(
                    &wake.command_id,
                    &wake.command_digest,
                    &claim_token,
                    lease_expires_at_ms,
                    now_ms,
                )
                .map_err(|error| error.to_string())?
            else {
                return Err("controller wake claim was lost before recovery".to_string());
            };
            if claimed_wake.outbox_state != "claimed" || claimed_wake.outbox_attempt == 0 {
                return Err("controller wake claim did not retain ownership".to_string());
            }
            let recovery = HostInteractionRecoveryEnvelope {
                schema_version: HOST_INTERACTION_RECOVERY_SCHEMA.to_string(),
                record_id: record_id.clone(),
                checkpoint_key: current.checkpoint_key.clone(),
                run_id: current.root_run_id.clone(),
                trace_id: current.trace_id.clone(),
                claim_mode: "recovery".to_string(),
                resume_attempt: current.resume_attempt,
                expected_revision: current.revision,
                logical_cycle: request.logical_cycle,
                interaction_id: request.interaction_id.clone(),
                operation_id: request.operation_id.clone(),
                tool_call_id: request.tool_call_id.clone(),
                request_digest: request.request_digest.clone(),
                command_id: response_command_id,
            };
            let result = store
                .claim_and_consume_host_interaction_response(recovery)
                .map_err(|error| error.to_string())?;
            result.validate().map_err(|error| error.to_string())?;
            if !matches!(result.kind.as_str(), "applied" | "replayed")
                || result.record_id != record_id
            {
                return Err("host interaction recovery did not consume the response".to_string());
            }
            let completed = store
                .complete_controller_command_wake(
                    &wake.command_id,
                    &wake.command_digest,
                    &claim_token,
                    claimed_wake.outbox_attempt,
                    "delivered",
                    now_unix_ms()?,
                    None,
                )
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "controller wake disappeared during completion".to_string())?;
            if completed.outbox_state != "delivered" {
                return Err("controller wake completion was not durable".to_string());
            }
            current = load_checkpoint_once(store, &current.checkpoint_key)?;
            consumed_host_response = true;
        } else {
            let Some(claimed_wake) = store
                .claim_controller_command_wake(
                    &wake.command_id,
                    &wake.command_digest,
                    &claim_token,
                    lease_expires_at_ms,
                    now_ms,
                )
                .map_err(|error| error.to_string())?
            else {
                return Err("controller wake claim was lost before completion".to_string());
            };
            if claimed_wake.outbox_state != "claimed" || claimed_wake.outbox_attempt == 0 {
                return Err("controller wake claim did not retain ownership".to_string());
            }
            let completed = store
                .complete_controller_command_wake(
                    &wake.command_id,
                    &wake.command_digest,
                    &claim_token,
                    claimed_wake.outbox_attempt,
                    "delivered",
                    now_unix_ms()?,
                    None,
                )
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "controller wake disappeared during completion".to_string())?;
            if completed.outbox_state != "delivered" {
                return Err("controller wake completion was not durable".to_string());
            }
            current = load_checkpoint_once(store, &current.checkpoint_key)?;
        }
    }
    Ok((current, consumed_host_response))
}

fn host_request_from_event(
    checkpoint: &crate::runtime::Checkpoint,
    interaction_id: &str,
    request_digest: &str,
) -> Result<HostInteractionRequest, String> {
    for entry in &checkpoint.event_outbox {
        let event = serde_json::from_value::<RunEvent>(entry.event.clone())
            .map_err(|error| format!("host interaction recovery event is invalid: {error}"))?;
        let RunEventPayload::HostInteractionRequested {
            checkpoint_key,
            interaction_id: event_interaction_id,
            logical_cycle,
            operation_id,
            tool_call_id,
            request_digest: event_request_digest,
            prompt,
            ..
        } = event.payload
        else {
            continue;
        };
        if checkpoint_key == checkpoint.checkpoint_key
            && event_interaction_id == interaction_id
            && event_request_digest == request_digest
        {
            let request = HostInteractionRequest {
                schema_version: HOST_INTERACTION_REQUEST_SCHEMA.to_string(),
                interaction_id: event_interaction_id,
                logical_cycle,
                operation_id,
                tool_call_id,
                request_digest: event_request_digest,
                prompt,
            };
            request.validate().map_err(|error| error.to_string())?;
            return Ok(request);
        }
    }
    Err("host interaction recovery request is missing".to_string())
}

fn checkpoint_has_unconsumed_host_interaction(
    checkpoint: &crate::runtime::Checkpoint,
) -> Result<bool, String> {
    let mut requested = std::collections::BTreeSet::new();
    let mut consumed = std::collections::BTreeSet::new();
    for entry in &checkpoint.event_outbox {
        let event = serde_json::from_value::<RunEvent>(entry.event.clone())
            .map_err(|error| format!("host interaction recovery event is invalid: {error}"))?;
        match event.payload {
            RunEventPayload::HostInteractionRequested {
                interaction_id,
                request_digest,
                ..
            } => {
                requested.insert((interaction_id, request_digest));
            }
            RunEventPayload::HostInteractionResponseConsumed {
                interaction_id,
                request_digest,
                ..
            } => {
                consumed.insert((interaction_id, request_digest));
            }
            _ => {}
        }
    }
    Ok(requested.into_iter().any(|key| !consumed.contains(&key)))
}

fn validate_checkpoint_identity(
    envelope: &DistributedRunEnvelope,
    checkpoint: &crate::runtime::Checkpoint,
) -> Result<(), String> {
    if checkpoint.checkpoint_key != envelope.checkpoint_config.key
        || checkpoint.task_id != envelope.task.task_id
        || checkpoint.root_run_id != envelope.root_run_id
        || checkpoint.trace_id != envelope.trace_id
        || checkpoint.run_definition_digest != envelope.run_definition_digest
    {
        return Err(
            "distributed advance envelope does not match the authoritative checkpoint".to_string(),
        );
    }
    Ok(())
}
