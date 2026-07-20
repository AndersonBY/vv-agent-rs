use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::config::build_vv_llm_from_local_settings;
use crate::context::RunContext;
use crate::llm::LlmClient;
use crate::model::ModelRef;
use crate::runtime::backends::InlineBackend;
use crate::runtime::context::ExecutionContext;
use crate::runtime::engine::{AgentRuntime, RuntimeEventHandler, RuntimeRunControls};
use crate::runtime::state::{Checkpoint, LeaseOperationClock, StateStore};
use crate::runtime::tool_planner::{project_tool_policy, projected_metadata_denials};
use crate::types::{AgentResult, AgentStatus, Metadata};
use crate::workspace::LocalWorkspaceBackend;

use super::capabilities::{DistributedCapabilityRegistry, ResolvedDistributedCapabilities};
use super::contract::{now_unix_ms, DistributedRunEnvelope};
use super::dispatch::CycleDispatchResult;
use super::worker_v2::{
    run_distributed_cycle_v2, DistributedDeliveryMetadata, DistributedV2CycleExecutor,
};

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
    #[cfg(test)]
    failure_recorded: Arc<Condvar>,
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

struct LeaseRenewalRequest {
    now_ms: u64,
    lease_expires_at_ms: u64,
    clock: LeaseOperationClock,
}

impl LeaseHeartbeatStatus {
    pub(super) fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(LeaseHeartbeatState {
                failure: None,
                commit_phase: LeaseCommitPhase::NotStarted,
            })),
            #[cfg(test)]
            failure_recorded: Arc::new(Condvar::new()),
        }
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
            #[cfg(test)]
            self.failure_recorded.notify_all();
        }
    }

    #[cfg(test)]
    fn wait_for_failure(&self, timeout: Duration) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (state, _) = self
            .failure_recorded
            .wait_timeout_while(state, timeout, |state| state.failure.is_none())
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.failure.is_some()
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

    #[cfg(test)]
    fn uncommitted(value: T) -> Self {
        Self::new(value, false)
    }
}

#[derive(Clone)]
pub struct DistributedCycleWorker {
    pub(super) capabilities: DistributedCapabilityRegistry,
    pub(super) checkpoint_executor: Option<Arc<dyn DistributedV2CycleExecutor>>,
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

    pub fn with_checkpoint_executor(
        mut self,
        executor: Arc<dyn DistributedV2CycleExecutor>,
    ) -> Self {
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
        envelope.ensure_not_expired()?;
        if envelope.is_checkpoint_v2() {
            return run_distributed_cycle_v2(self, envelope, delivery);
        }
        self.run_cycle_v1(envelope)
    }

    fn run_cycle_v1(
        &self,
        envelope: DistributedRunEnvelope,
    ) -> Result<CycleDispatchResult, String> {
        let state_store = envelope
            .recipe
            .build_state_store()
            .map_err(|error| error.to_string())?;
        if let Some(checkpoint) = state_store
            .load_checkpoint(&envelope.task.task_id)
            .map_err(|error| error.to_string())?
        {
            if let Some(result) = checkpoint.terminal_result {
                return Ok(CycleDispatchResult::finished_at_revision(
                    result,
                    Some(checkpoint.revision),
                ));
            }
        }

        // Resolve the complete capability graph before claiming work or calling the model.
        let mut resolved = self
            .capabilities
            .resolve(&envelope.recipe.capabilities)
            .map_err(|error| error.to_string())?;
        let projected_policy = projected_metadata_denials(&envelope.task)?;
        resolved
            .tool_policy
            .extend_metadata_denials(&projected_policy);
        let runtime = build_runtime(&envelope, &resolved)?;
        let heartbeat_state_store = envelope
            .recipe
            .build_state_store()
            .map_err(|error| error.to_string())?;
        let now_ms = now_unix_ms()?;
        envelope.ensure_not_expired_at(now_ms)?;
        let lease_expires_at_ms = lease_expiry_at(
            now_ms,
            envelope.lease_duration_ms,
            envelope.deadline_unix_ms,
        )?;
        let claim_token = uuid::Uuid::new_v4().simple().to_string();
        let Some(mut checkpoint) = state_store
            .claim_checkpoint(
                &envelope.task.task_id,
                envelope.cycle_index,
                &claim_token,
                lease_expires_at_ms,
                now_ms,
            )
            .map_err(|error| format!("retryable distributed delivery conflict: {error}"))?
        else {
            return Ok(CycleDispatchResult::finished(failed_result(
                format!("No checkpoint found for task {}", envelope.task.task_id),
                Vec::new(),
                Vec::new(),
                Metadata::new(),
            )));
        };

        let previous_cycle_count = checkpoint.cycles.len();
        let controls = worker_controls(&envelope, &resolved, &checkpoint, state_store.clone());
        let mut worker_task = envelope.task.clone();
        project_tool_policy(&mut worker_task, &resolved.tool_policy);
        let cycle_result = run_with_lease_heartbeat(
            heartbeat_state_store,
            &envelope,
            &claim_token,
            checkpoint.revision,
            |heartbeat_status| {
                let cycle_result = (|| -> Result<CycleDispatchResult, String> {
                    let runtime_result = runtime.run_with_controls(worker_task, controls);
                    let result = runtime_result.unwrap_or_else(|error| {
                        failed_result(
                            error.to_string(),
                            checkpoint.messages.clone(),
                            checkpoint.cycles.clone(),
                            checkpoint.shared_state.clone(),
                        )
                    });

                    checkpoint.cycle_index = envelope.cycle_index;
                    checkpoint.messages = result.messages.clone();
                    checkpoint.cycles = result.cycles.clone();
                    checkpoint.shared_state = result.shared_state.clone();
                    checkpoint.budget_usage = result.budget_usage.clone();
                    let expected_revision = checkpoint.revision;
                    heartbeat_status.begin_commit()?;
                    if result.status == AgentStatus::MaxCycles
                        && result.cycles.len() > previous_cycle_count
                    {
                        checkpoint.status = AgentStatus::Running;
                        checkpoint.terminal_result = None;
                        if !state_store
                            .commit_checkpoint(checkpoint, &claim_token, expected_revision)
                            .map_err(|error| error.to_string())?
                        {
                            return Err(format!(
                                "checkpoint changed while cycle {} was running for task {}",
                                envelope.cycle_index, envelope.task.task_id
                            ));
                        }
                        heartbeat_status.mark_commit_succeeded()?;
                        return Ok(CycleDispatchResult::unfinished());
                    }

                    checkpoint.status = result.status;
                    checkpoint.terminal_result = Some(result.clone());
                    if !state_store
                        .commit_checkpoint(checkpoint, &claim_token, expected_revision)
                        .map_err(|error| error.to_string())?
                    {
                        return Err(format!(
                            "checkpoint changed while terminal cycle {} was running for task {}",
                            envelope.cycle_index, envelope.task.task_id
                        ));
                    }
                    heartbeat_status.mark_commit_succeeded()?;
                    Ok(CycleDispatchResult::finished_at_revision(
                        result,
                        Some(expected_revision + 1),
                    ))
                })();
                let claim_committed = cycle_result.is_ok();
                LeaseOperationResult::new(cycle_result, claim_committed)
            },
        )?;
        cycle_result
    }
}

fn run_with_lease_heartbeat<T>(
    state_store: Arc<dyn StateStore>,
    envelope: &DistributedRunEnvelope,
    claim_token: &str,
    expected_revision: u64,
    operation: impl FnOnce(&LeaseHeartbeatStatus) -> LeaseOperationResult<T>,
) -> Result<T, String> {
    run_with_checkpoint_lease(
        state_store.as_ref(),
        &envelope.task.task_id,
        envelope.cycle_index,
        envelope.lease_duration_ms,
        envelope.deadline_unix_ms,
        claim_token,
        expected_revision,
        operation,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run_with_checkpoint_lease<T>(
    state_store: &dyn StateStore,
    task_id: &str,
    cycle_index: u32,
    lease_duration_ms: u64,
    deadline_unix_ms: Option<u64>,
    claim_token: &str,
    expected_revision: u64,
    operation: impl FnOnce(&LeaseHeartbeatStatus) -> LeaseOperationResult<T>,
) -> Result<T, String> {
    let stopped = Arc::new((Mutex::new(false), Condvar::new()));
    let heartbeat_status = LeaseHeartbeatStatus::new();
    let task_id = task_id.to_string();
    let claim_token = claim_token.to_string();

    let known_lease_expires_at_ms = load_claim_lease_expiry(
        state_store,
        &task_id,
        &claim_token,
        expected_revision,
        cycle_index,
    )
    .map_err(|failure| format!("checkpoint lease heartbeat failed: {}", failure.message))?;
    let initial_request = prepare_lease_renewal(&task_id, lease_duration_ms, deadline_unix_ms)
        .map_err(|failure| format!("checkpoint lease heartbeat failed: {}", failure.message))?;
    let initial_renewal = renew_checkpoint_lease(
        state_store,
        &task_id,
        &claim_token,
        expected_revision,
        initial_request,
        known_lease_expires_at_ms,
    )
    .map_err(|failure| format!("checkpoint lease heartbeat failed: {}", failure.message))?;

    let result = std::thread::scope(|scope| {
        let stopped_for_thread = stopped.clone();
        let status_for_thread = heartbeat_status.clone();
        let heartbeat = scope.spawn(move || {
            let mut known_lease_expires_at_ms = initial_renewal.lease_expires_at_ms;
            let mut interval = lease_heartbeat_interval(initial_renewal.effective_lease_ms);
            loop {
                let (lock, changed) = &*stopped_for_thread;
                let guard = lock
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let (guard, _) = changed
                    .wait_timeout_while(guard, interval, |stopped| !*stopped)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if *guard {
                    break;
                }
                drop(guard);

                let request =
                    match prepare_lease_renewal(&task_id, lease_duration_ms, deadline_unix_ms) {
                        Ok(request) => request,
                        Err(failure) => {
                            status_for_thread.record(failure, LeaseCommitPhase::NotStarted);
                            break;
                        }
                    };
                let phase_at_start = status_for_thread.commit_phase();
                let outcome = renew_checkpoint_lease(
                    state_store,
                    &task_id,
                    &claim_token,
                    expected_revision,
                    request,
                    known_lease_expires_at_ms,
                );
                match outcome {
                    Ok(renewal) => {
                        known_lease_expires_at_ms = renewal.lease_expires_at_ms;
                        interval = lease_heartbeat_interval(renewal.effective_lease_ms);
                    }
                    Err(failure) => {
                        status_for_thread.record(failure, phase_at_start);
                        break;
                    }
                }
            }
        });

        let stop_guard = LeaseHeartbeatStopGuard::new(stopped.clone());
        let result = operation(&heartbeat_status);
        drop(stop_guard);
        heartbeat
            .join()
            .map_err(|_| "checkpoint lease heartbeat panicked".to_string())?;
        Ok::<_, String>(result)
    })?;

    let (failure, commit_phase) = heartbeat_status.take();
    if let Some(failure) = failure {
        let commit_consumed_claim = result.claim_committed
            && commit_phase == LeaseCommitPhase::Succeeded
            && failure.renewal_started_during_commit
            && failure.renewal.kind == LeaseRenewalFailureKind::ActiveClaimLost;
        if !commit_consumed_claim {
            return Err(format!(
                "checkpoint lease heartbeat failed: {}",
                failure.renewal.message
            ));
        }
    }
    Ok(result.value)
}

fn load_claim_lease_expiry(
    state_store: &dyn StateStore,
    task_id: &str,
    claim_token: &str,
    expected_revision: u64,
    expected_cycle: u32,
) -> Result<u64, LeaseRenewalFailure> {
    let checkpoint = state_store
        .load_checkpoint(task_id)
        .map_err(|error| LeaseRenewalFailure::coordination(error.to_string()))?;
    checkpoint
        .filter(|checkpoint| {
            checkpoint.revision == expected_revision
                && checkpoint.claim_token.as_deref() == Some(claim_token)
                && checkpoint.claimed_cycle == Some(expected_cycle)
        })
        .and_then(|checkpoint| checkpoint.lease_expires_at_ms)
        .ok_or_else(LeaseRenewalFailure::active_claim_lost)
}

fn prepare_lease_renewal(
    task_id: &str,
    lease_duration_ms: u64,
    deadline_unix_ms: Option<u64>,
) -> Result<LeaseRenewalRequest, LeaseRenewalFailure> {
    let now_ms = now_unix_ms().map_err(LeaseRenewalFailure::coordination)?;
    let clock = LeaseOperationClock::new(now_ms);
    if deadline_unix_ms.is_some_and(|deadline| deadline <= now_ms) {
        return Err(LeaseRenewalFailure::coordination(format!(
            "distributed job deadline expired while renewing {task_id}"
        )));
    }
    let lease_expires_at_ms = lease_expiry_at(now_ms, lease_duration_ms, deadline_unix_ms)
        .map_err(LeaseRenewalFailure::coordination)?;
    Ok(LeaseRenewalRequest {
        now_ms,
        lease_expires_at_ms,
        clock,
    })
}

fn renew_checkpoint_lease(
    state_store: &dyn StateStore,
    task_id: &str,
    claim_token: &str,
    expected_revision: u64,
    request: LeaseRenewalRequest,
    known_lease_expires_at_ms: u64,
) -> Result<LeaseRenewal, LeaseRenewalFailure> {
    let renewed = state_store
        .renew_checkpoint_claim(
            task_id,
            claim_token,
            expected_revision,
            request.lease_expires_at_ms,
            request.now_ms,
        )
        .map_err(|error| {
            let message = error.to_string();
            if message == "claim lease expired" {
                LeaseRenewalFailure::claim_lease_expired()
            } else {
                LeaseRenewalFailure::coordination(message)
            }
        })?;
    let observed_at_ms = request
        .clock
        .now_ms()
        .max(now_unix_ms().map_err(LeaseRenewalFailure::coordination)?);
    if !renewed {
        return Err(
            if observed_at_ms >= known_lease_expires_at_ms
                || observed_at_ms >= request.lease_expires_at_ms
            {
                LeaseRenewalFailure::claim_lease_expired()
            } else {
                LeaseRenewalFailure::active_claim_lost()
            },
        );
    }
    if observed_at_ms >= known_lease_expires_at_ms || observed_at_ms >= request.lease_expires_at_ms
    {
        return Err(LeaseRenewalFailure::claim_lease_expired());
    }
    Ok(LeaseRenewal {
        lease_expires_at_ms: request.lease_expires_at_ms,
        effective_lease_ms: request.lease_expires_at_ms - observed_at_ms,
    })
}

fn lease_heartbeat_interval(lease_duration_ms: u64) -> Duration {
    let interval_micros = lease_duration_ms
        .saturating_mul(1_000)
        .saturating_div(3)
        .clamp(1, 30_000_000);
    Duration::from_micros(interval_micros)
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

fn worker_controls(
    envelope: &DistributedRunEnvelope,
    resolved: &ResolvedDistributedCapabilities,
    checkpoint: &Checkpoint,
    state_store: Arc<dyn StateStore>,
) -> RuntimeRunControls {
    let mut metadata = envelope.task.metadata.clone();
    metadata.insert(
        "_vv_agent_run_id".to_string(),
        serde_json::Value::String(envelope.run_id.clone()),
    );
    let mut execution_context = ExecutionContext {
        cancellation_token: resolved.cancellation.clone(),
        state_store: Some(state_store),
        approval_provider: resolved.approval_provider.clone(),
        approval_broker: resolved.approval_broker.clone(),
        approval_timeout: resolved
            .approval_timeout_seconds
            .map(Duration::from_secs_f64),
        memory_providers: resolved.memory_providers.clone(),
        app_state: resolved.app_state.clone(),
        metadata,
        ..ExecutionContext::default()
    };
    if execution_context.approval_provider.is_some() && execution_context.approval_broker.is_none()
    {
        execution_context.approval_broker = Some(Default::default());
    }
    RuntimeRunControls {
        log_handler: combined_event_handler(resolved),
        cancellation_token: resolved.cancellation.clone(),
        execution_context: Some(execution_context),
        workspace: Some(PathBuf::from(&envelope.recipe.workspace)),
        workspace_backend: resolved.workspace_backend.clone(),
        run_context: Some(RunContext {
            run_id: envelope.run_id.clone(),
            model: Some(ModelRef::backend(
                envelope.recipe.backend.clone(),
                envelope.recipe.model.clone(),
            )),
            workspace: Some(PathBuf::from(&envelope.recipe.workspace)),
            app_state: resolved.app_state.clone(),
            ..RunContext::default()
        }),
        sub_task_manager: resolved.sub_task_manager.clone(),
        budget_limits: envelope.budget_limits.clone(),
        host_cost_meter: resolved.host_cost_meter.clone(),
        initial_messages: Some(checkpoint.messages.clone()),
        initial_shared_state: Some(checkpoint.shared_state.clone()),
        initial_cycles: Some(checkpoint.cycles.clone()),
        cycle_index_start: Some(envelope.cycle_index),
        cycle_count: Some(1),
        initial_budget_usage: checkpoint.budget_usage.clone(),
        defer_terminal_on_max_cycles: true,
        ..RuntimeRunControls::default()
    }
}

pub(super) fn combined_event_handler(
    resolved: &ResolvedDistributedCapabilities,
) -> Option<RuntimeEventHandler> {
    let mut handlers = resolved.observers.clone();
    if let Some(event_sink) = &resolved.event_sink {
        handlers.push(event_sink.clone());
    }
    if handlers.is_empty() {
        return None;
    }
    Some(Arc::new(move |event, payload| {
        for handler in &handlers {
            handler(event, payload);
        }
    }))
}

fn failed_result(
    error: String,
    messages: Vec<crate::types::Message>,
    cycles: Vec<crate::types::CycleRecord>,
    shared_state: Metadata,
) -> AgentResult {
    let token_usage = crate::runtime::summarize_task_token_usage(&cycles);
    let partial_output = crate::types::last_assistant_output(&cycles);
    AgentResult {
        status: AgentStatus::Failed,
        messages,
        cycles,
        completion_reason: Some(crate::types::CompletionReason::Failed),
        completion_tool_name: None,
        partial_output,
        budget_usage: None,
        budget_exhaustion: None,
        checkpoint_key: None,
        resume_observation: None,
        final_answer: None,
        wait_reason: None,
        error: Some(error),
        shared_state,
        token_usage,
    }
}

#[cfg(test)]
mod tests;
