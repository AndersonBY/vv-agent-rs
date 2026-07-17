use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::checkpoint::{CheckpointConfig, EventCursor, IdempotentRunEventStore};
use crate::checkpoint::{
    CheckpointStatus, ClaimMode, OperationKind, OperationState, ReconciliationDecision,
    ReconciliationDecisionKind, ResumeObservation, ToolIdempotency,
};
use crate::event_store::{EventStoreError, RunEventIter, RunEventReplayQuery, RunEventStore};
use crate::events::RunEvent;
use crate::runtime::checkpoint_resume::{
    CheckpointControllerRequest, CheckpointEventSink, CheckpointResumeController,
};
use crate::runtime::run_definition_v2::validate_distributed_run_definition;
use crate::runtime::state_v2::{
    validate_extension_state_size, CheckpointStoreV2, CheckpointV2, ExtensionStateEntry,
    OperationError,
};
use crate::runtime::tool_planner::project_tool_policy;
use crate::runtime::{CheckpointRuntimeControl, ExecutionContext, RuntimeRunControls};
use crate::types::AgentResult;
use crate::types::AgentStatus;
use crate::{ModelRef, RunContext};

use super::capabilities::ResolvedDistributedCapabilities;
use super::contract::{now_unix_ms, DistributedCheckpointConfig, DistributedRunEnvelope};
use super::dispatch::CycleDispatchResult;
use super::worker::{
    build_runtime, combined_event_handler, lease_expiry_at, DistributedCycleWorker,
    LeaseCommitPhase, LeaseHeartbeatStatus, LeaseHeartbeatStopGuard, LeaseOperationResult,
    LeaseRenewal, LeaseRenewalFailure, LeaseRenewalFailureKind,
};

mod lease;
mod recovery;
mod runtime;

use lease::run_with_checkpoint_lease_v2;
use recovery::{
    align_active_claim, commit_cycle, effective_claim_mode, initialize_extensions, load_v2,
    prepare_terminal_candidate, reconcile_recovery, reconciliation_candidate, snapshot_extensions,
    suspend_reconciliation, terminal_replay, validate_claimed_resume_attempt,
    validate_envelope_checkpoint_identity, validate_extension_capabilities,
    validate_resume_attempt_observation,
};
use runtime::run_agent_runtime_cycle_v2;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DistributedDeliveryMetadata {
    pub redelivered: bool,
    pub attempt: u64,
}

impl DistributedDeliveryMetadata {
    pub fn redelivery(attempt: u64) -> Self {
        Self {
            redelivered: true,
            attempt,
        }
    }

    pub fn is_redelivery(self) -> bool {
        self.redelivered || self.attempt > 1
    }
}

pub trait DistributedV2CycleExecutor: Send + Sync {
    fn execute(
        &self,
        envelope: &DistributedRunEnvelope,
        capabilities: &ResolvedDistributedCapabilities,
        checkpoint: &mut DistributedCheckpointProgress,
    ) -> Result<DistributedV2CycleOutcome, String>;
}

#[derive(Debug, Clone, PartialEq)]
pub enum DistributedV2CycleOutcome {
    Continue(CheckpointV2),
    ReconciliationRequired(CheckpointV2),
    Terminal(CheckpointV2),
}

pub struct DistributedCheckpointProgress {
    store: Arc<dyn CheckpointStoreV2>,
    claim_token: String,
    checkpoint: CheckpointV2,
}

impl DistributedCheckpointProgress {
    fn new(
        store: Arc<dyn CheckpointStoreV2>,
        claim_token: String,
        checkpoint: CheckpointV2,
    ) -> Self {
        Self {
            store,
            claim_token,
            checkpoint,
        }
    }

    pub fn checkpoint(&self) -> &CheckpointV2 {
        &self.checkpoint
    }

    pub fn claim_token(&self) -> &str {
        &self.claim_token
    }

    pub fn persist(&mut self, mut snapshot: CheckpointV2) -> Result<CheckpointV2, String> {
        align_active_claim(&mut snapshot, &self.checkpoint);
        let expected_revision = self.checkpoint.revision;
        if !self
            .store
            .progress_checkpoint_v2(snapshot, &self.claim_token, expected_revision)
            .map_err(|error| error.to_string())?
        {
            return Err(format!(
                "checkpoint progress conflict at revision {expected_revision} for {}",
                self.checkpoint.checkpoint_key
            ));
        }
        self.reload()?;
        Ok(self.checkpoint.clone())
    }

    fn reload(&mut self) -> Result<(), String> {
        let checkpoint = self
            .store
            .load_checkpoint_v2(&self.checkpoint.checkpoint_key)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| {
                format!(
                    "No checkpoint found for key {}",
                    self.checkpoint.checkpoint_key
                )
            })?;
        if checkpoint.claim_token.as_deref() != Some(self.claim_token.as_str()) {
            return Err(format!(
                "checkpoint claim changed while progressing {}",
                checkpoint.checkpoint_key
            ));
        }
        self.checkpoint = checkpoint;
        Ok(())
    }
}

enum RecoveryDisposition {
    Continue,
    Suspend,
    Abort(Box<CheckpointV2>),
}

enum PostCommitAction {
    Unfinished {
        revision: u64,
        cycle_index: u64,
    },
    TerminalCandidate {
        result: Box<AgentResult>,
        revision: u64,
    },
}

pub(super) fn run_distributed_cycle_v2(
    worker: &DistributedCycleWorker,
    envelope: DistributedRunEnvelope,
    delivery: DistributedDeliveryMetadata,
) -> Result<CycleDispatchResult, String> {
    let checkpoint_store_ref = envelope
        .recipe
        .capabilities
        .checkpoint_store_ref
        .as_ref()
        .ok_or_else(|| "distributed v2 requires checkpoint_store_ref".to_string())?;
    let store = worker
        .capabilities
        .resolve_checkpoint_store_required(checkpoint_store_ref)
        .map_err(|error| error.to_string())?;
    let config = envelope
        .checkpoint_config
        .as_ref()
        .expect("validated v2 envelope has checkpoint_config");
    let checkpoint_key = config.key.as_str();
    let now_ms = now_unix_ms()?;
    let checkpoint = load_v2(store.as_ref(), checkpoint_key)?;
    validate_envelope_checkpoint_identity(&envelope, &checkpoint)?;
    validate_distributed_run_definition(&envelope, &checkpoint, None)
        .map_err(|error| error.to_string())?;

    if checkpoint.terminal_result.is_some() {
        return terminal_replay(&checkpoint);
    }
    if checkpoint.cycle_index >= u64::from(envelope.cycle_index) && checkpoint.claim_token.is_none()
    {
        return Ok(CycleDispatchResult::committed(
            checkpoint.cycle_index,
            checkpoint.revision,
        ));
    }
    validate_resume_attempt_observation(&envelope, &checkpoint, delivery)?;
    if checkpoint
        .lease_expires_at_ms
        .is_some_and(|lease| lease > now_ms)
    {
        return Ok(CycleDispatchResult::unfinished());
    }

    let resolved = worker
        .capabilities
        .resolve(&envelope.recipe.capabilities)
        .map_err(|error| error.to_string())?;
    validate_distributed_run_definition(&envelope, &checkpoint, Some(&resolved))
        .map_err(|error| error.to_string())?;

    validate_extension_capabilities(config, &resolved)?;
    if worker.checkpoint_executor.is_none() {
        return run_agent_runtime_cycle_v2(envelope, delivery, resolved, store, checkpoint);
    }
    let executor = worker
        .checkpoint_executor
        .clone()
        .expect("checkpoint executor checked above");

    let claim_mode = effective_claim_mode(&envelope, &checkpoint, delivery, now_ms);
    let lease_expires_at_ms = lease_expiry_at(
        now_ms,
        envelope.lease_duration_ms,
        envelope.deadline_unix_ms,
    )?;
    let claim_token = uuid::Uuid::new_v4().simple().to_string();
    let resume_attempt_before_claim = checkpoint.resume_attempt;
    let Some(claimed) = store
        .claim_checkpoint_v2(
            checkpoint_key,
            u64::from(envelope.cycle_index),
            &claim_token,
            lease_expires_at_ms,
            now_ms,
            claim_mode,
        )
        .map_err(|error| format!("retryable distributed v2 delivery conflict: {error}"))?
    else {
        let latest = load_v2(store.as_ref(), checkpoint_key)?;
        validate_envelope_checkpoint_identity(&envelope, &latest)?;
        if latest.terminal_result.is_some() {
            return terminal_replay(&latest);
        }
        return Ok(CycleDispatchResult::unfinished());
    };
    validate_claimed_resume_attempt(resume_attempt_before_claim, &claimed, claim_mode)?;

    let action = run_with_checkpoint_lease_v2(
        store.clone(),
        checkpoint_key,
        u64::from(envelope.cycle_index),
        envelope.lease_duration_ms,
        envelope.deadline_unix_ms,
        &claim_token,
        |heartbeat_status| {
            let result = (|| -> Result<PostCommitAction, String> {
                let mut progress =
                    DistributedCheckpointProgress::new(store.clone(), claim_token.clone(), claimed);
                initialize_extensions(config, &resolved, &mut progress)?;

                if claim_mode == ClaimMode::Recovery {
                    match reconcile_recovery(config, &resolved, &mut progress)? {
                        RecoveryDisposition::Continue => {}
                        RecoveryDisposition::Suspend => {
                            suspend_reconciliation(&mut progress, heartbeat_status)?;
                            let checkpoint = load_v2(store.as_ref(), checkpoint_key)?;
                            let result = reconciliation_candidate(&checkpoint)?;
                            return Ok(PostCommitAction::TerminalCandidate {
                                result: Box::new(result),
                                revision: checkpoint.revision,
                            });
                        }
                        RecoveryDisposition::Abort(checkpoint) => {
                            let (result, revision) = prepare_terminal_candidate(
                                *checkpoint,
                                &mut progress,
                                u64::from(envelope.cycle_index),
                            )?;
                            return Ok(PostCommitAction::TerminalCandidate {
                                result: Box::new(result),
                                revision,
                            });
                        }
                    }
                }

                let outcome = executor.execute(&envelope, &resolved, &mut progress)?;
                match outcome {
                    DistributedV2CycleOutcome::Continue(mut checkpoint) => {
                        snapshot_extensions(config, &resolved, &mut checkpoint)?;
                        commit_cycle(
                            checkpoint,
                            &mut progress,
                            heartbeat_status,
                            u64::from(envelope.cycle_index),
                        )?;
                        let committed = load_v2(store.as_ref(), checkpoint_key)?;
                        Ok(PostCommitAction::Unfinished {
                            revision: committed.revision,
                            cycle_index: committed.cycle_index,
                        })
                    }
                    DistributedV2CycleOutcome::ReconciliationRequired(mut checkpoint) => {
                        snapshot_extensions(config, &resolved, &mut checkpoint)?;
                        align_active_claim(&mut checkpoint, &progress.checkpoint);
                        progress.checkpoint = checkpoint;
                        suspend_reconciliation(&mut progress, heartbeat_status)?;
                        let checkpoint = load_v2(store.as_ref(), checkpoint_key)?;
                        let result = reconciliation_candidate(&checkpoint)?;
                        Ok(PostCommitAction::TerminalCandidate {
                            result: Box::new(result),
                            revision: checkpoint.revision,
                        })
                    }
                    DistributedV2CycleOutcome::Terminal(mut checkpoint) => {
                        snapshot_extensions(config, &resolved, &mut checkpoint)?;
                        let (result, revision) = prepare_terminal_candidate(
                            checkpoint,
                            &mut progress,
                            u64::from(envelope.cycle_index),
                        )?;
                        Ok(PostCommitAction::TerminalCandidate {
                            result: Box::new(result),
                            revision,
                        })
                    }
                }
            })();
            let claim_committed = match &result {
                Ok(PostCommitAction::Unfinished { .. }) => true,
                Ok(PostCommitAction::TerminalCandidate { result, .. }) => {
                    result.status == crate::types::AgentStatus::ReconciliationRequired
                }
                Err(_) => false,
            };
            LeaseOperationResult::new(result, claim_committed)
        },
    )??;

    match action {
        PostCommitAction::Unfinished {
            revision,
            cycle_index,
        } => Ok(CycleDispatchResult::committed(cycle_index, revision)),
        PostCommitAction::TerminalCandidate { result, revision } => {
            Ok(CycleDispatchResult::terminal_candidate(*result, revision))
        }
    }
}
