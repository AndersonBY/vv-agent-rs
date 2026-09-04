use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Condvar, Mutex,
};

use crate::config::build_vv_llm_from_local_settings;
use crate::llm::LlmClient;
use crate::runtime::backends::InlineBackend;
use crate::runtime::engine::{AgentRuntime, RunEventHandler};
use crate::workspace::LocalWorkspaceBackend;

use super::capabilities::{DistributedCapabilityRegistry, ResolvedDistributedCapabilities};
use super::checkpoint_worker::{
    run_distributed_cycle, DistributedCycleExecutor, DistributedDeliveryMetadata,
};
use super::contract::now_unix_ms;
use super::contract::DistributedRunEnvelope;
use super::dispatch::CycleDispatchResult;

pub(super) struct LeaseHeartbeatStopGuard {
    stopped: Arc<(Mutex<bool>, Condvar)>,
}

impl LeaseHeartbeatStopGuard {
    pub(super) fn new(stopped: Arc<(Mutex<bool>, Condvar)>) -> Self {
        Self { stopped }
    }
}

impl Drop for LeaseHeartbeatStopGuard {
    fn drop(&mut self) {
        let (lock, changed) = &*self.stopped;
        *lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        changed.notify_all();
    }
}

#[derive(Clone)]
pub(super) struct LeaseHeartbeatStatus {
    state: Arc<Mutex<LeaseHeartbeatState>>,
    lease_expires_at_ms: Arc<AtomicU64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LeaseCommitPhase {
    NotStarted,
    InProgress,
    Succeeded,
}

pub(super) struct LeaseHeartbeatFailure {
    pub(super) renewal: LeaseRenewalFailure,
    pub(super) renewal_started_during_commit: bool,
}

struct LeaseHeartbeatState {
    failure: Option<LeaseHeartbeatFailure>,
    commit_phase: LeaseCommitPhase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LeaseRenewalFailureKind {
    ActiveClaimLost,
    ClaimLeaseExpired,
    Coordination,
}

pub(super) struct LeaseRenewalFailure {
    pub(super) kind: LeaseRenewalFailureKind,
    pub(super) message: String,
}

impl LeaseRenewalFailure {
    pub(super) fn active_claim_lost() -> Self {
        Self {
            kind: LeaseRenewalFailureKind::ActiveClaimLost,
            message: "claim is no longer active".to_string(),
        }
    }

    pub(super) fn claim_lease_expired() -> Self {
        Self {
            kind: LeaseRenewalFailureKind::ClaimLeaseExpired,
            message: "claim lease expired".to_string(),
        }
    }

    pub(super) fn coordination(message: String) -> Self {
        Self {
            kind: LeaseRenewalFailureKind::Coordination,
            message,
        }
    }
}

pub(super) struct LeaseRenewal {
    pub(super) lease_expires_at_ms: u64,
    pub(super) effective_lease_ms: u64,
}

impl LeaseHeartbeatStatus {
    pub(super) fn new(lease_expires_at_ms: u64) -> Self {
        Self {
            state: Arc::new(Mutex::new(LeaseHeartbeatState {
                failure: None,
                commit_phase: LeaseCommitPhase::NotStarted,
            })),
            lease_expires_at_ms: Arc::new(AtomicU64::new(lease_expires_at_ms)),
        }
    }

    pub(super) fn update_lease_expiry(&self, lease_expires_at_ms: u64) {
        let _ = self
            .lease_expires_at_ms
            .fetch_max(lease_expires_at_ms, Ordering::Release);
    }

    pub(super) fn commit_phase(&self) -> LeaseCommitPhase {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .commit_phase
    }

    pub(super) fn record(&self, renewal: LeaseRenewalFailure, phase_at_start: LeaseCommitPhase) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.failure.is_none() {
            state.failure = Some(LeaseHeartbeatFailure {
                renewal,
                renewal_started_during_commit: phase_at_start != LeaseCommitPhase::NotStarted,
            });
        }
    }

    pub(super) fn begin_commit(&self) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(failure) = &state.failure {
            return Err(format!(
                "checkpoint lease heartbeat failed: {}",
                failure.renewal.message
            ));
        }
        let now = now_unix_ms()?;
        if now >= self.lease_expires_at_ms.load(Ordering::Acquire) {
            return Err("checkpoint lease heartbeat failed: claim lease expired".to_string());
        }
        state.commit_phase = LeaseCommitPhase::InProgress;
        Ok(())
    }

    pub(super) fn mark_commit_succeeded(&self) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.commit_phase != LeaseCommitPhase::InProgress {
            return Err("checkpoint commit phase has not started".to_string());
        }
        state.commit_phase = LeaseCommitPhase::Succeeded;
        Ok(())
    }

    pub(super) fn take(&self) -> (Option<LeaseHeartbeatFailure>, LeaseCommitPhase) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (state.failure.take(), state.commit_phase)
    }
}

pub(super) struct LeaseOperationResult<T> {
    pub(super) value: T,
    pub(super) claim_committed: bool,
}

impl<T> LeaseOperationResult<T> {
    pub(super) fn new(value: T, claim_committed: bool) -> Self {
        Self {
            value,
            claim_committed,
        }
    }
}

#[derive(Clone)]
pub struct DistributedCycleWorker {
    pub(super) capabilities: DistributedCapabilityRegistry,
    pub(super) checkpoint_executor: Option<Arc<dyn DistributedCycleExecutor>>,
}

impl Default for DistributedCycleWorker {
    fn default() -> Self {
        Self::new(DistributedCapabilityRegistry::new())
    }
}

impl DistributedCycleWorker {
    pub fn new(capabilities: DistributedCapabilityRegistry) -> Self {
        Self {
            capabilities,
            checkpoint_executor: None,
        }
    }

    pub fn with_checkpoint_executor(mut self, executor: Arc<dyn DistributedCycleExecutor>) -> Self {
        self.checkpoint_executor = Some(executor);
        self
    }

    pub fn run_cycle(
        &self,
        envelope: DistributedRunEnvelope,
    ) -> Result<CycleDispatchResult, String> {
        self.run_cycle_with_delivery(envelope, DistributedDeliveryMetadata::default())
    }

    pub fn run_cycle_with_delivery(
        &self,
        envelope: DistributedRunEnvelope,
        delivery: DistributedDeliveryMetadata,
    ) -> Result<CycleDispatchResult, String> {
        envelope.validate()?;
        if envelope.recipe.capabilities.approval_provider_ref.is_some()
            || envelope.recipe.capabilities.approval_broker_ref.is_some()
        {
            return Err(
                "nonblocking distributed runs do not support brokered approval waits".to_string(),
            );
        }
        envelope.ensure_not_expired()?;
        run_distributed_cycle(self, envelope, delivery)
    }
}

pub(super) fn lease_expiry_at(
    now_ms: u64,
    lease_duration_ms: u64,
    deadline_unix_ms: Option<u64>,
) -> Result<u64, String> {
    let effective_duration = deadline_unix_ms
        .map(|deadline| lease_duration_ms.min(deadline.saturating_sub(now_ms)))
        .unwrap_or(lease_duration_ms);
    now_ms
        .checked_add(effective_duration)
        .ok_or_else(|| "checkpoint lease overflow".to_string())
}

pub(super) fn build_runtime(
    envelope: &DistributedRunEnvelope,
    resolved: &ResolvedDistributedCapabilities,
) -> Result<AgentRuntime<Arc<dyn LlmClient>>, String> {
    let llm_client = match resolved.llm_client.clone() {
        Some(client) => client,
        None => Arc::new(
            build_vv_llm_from_local_settings(
                &envelope.recipe.settings_file,
                &envelope.recipe.backend,
                &envelope.recipe.model,
                envelope.recipe.timeout_seconds,
            )
            .map_err(|error| error.to_string())?
            .0,
        ) as Arc<dyn LlmClient>,
    };
    let workspace = PathBuf::from(&envelope.recipe.workspace);
    let workspace_backend = resolved
        .workspace_backend
        .clone()
        .unwrap_or_else(|| Arc::new(LocalWorkspaceBackend::new(workspace.clone())));
    let mut runtime = AgentRuntime::new(llm_client)
        .with_tool_registry(resolved.tool_registry.clone())
        .with_execution_backend(InlineBackend)
        .with_default_workspace(workspace)
        .with_workspace_backend(workspace_backend)
        .with_settings_file(&envelope.recipe.settings_file)
        .with_default_backend(&envelope.recipe.backend)
        .with_hooks(resolved.hooks.clone())
        .with_after_cycle_hooks(resolved.after_cycle_hooks.clone());
    if let Some(log_preview_chars) = envelope.recipe.log_preview_chars {
        runtime = runtime.with_log_preview_chars(log_preview_chars);
    }
    runtime.set_tool_policy(resolved.tool_policy.clone());
    Ok(runtime)
}

pub(super) fn combined_event_handler(
    resolved: &ResolvedDistributedCapabilities,
) -> Option<RunEventHandler> {
    let mut handlers = resolved.observers.clone();
    if let Some(event_sink) = &resolved.event_sink {
        handlers.push(event_sink.clone());
    }
    if handlers.is_empty() {
        return None;
    }
    Some(Arc::new(move |event| {
        for handler in &handlers {
            handler(event);
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn begin_commit_rejects_expired_shared_lease_without_mutating_phase() {
        let heartbeat = LeaseHeartbeatStatus::new(u64::MIN);

        let error = heartbeat.begin_commit().unwrap_err();

        assert!(error.contains("claim lease expired"));
        assert_eq!(heartbeat.commit_phase(), LeaseCommitPhase::NotStarted);
    }
}
