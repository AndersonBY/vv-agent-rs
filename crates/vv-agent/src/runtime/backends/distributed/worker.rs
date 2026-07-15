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
use crate::runtime::state::{Checkpoint, StateStore};
use crate::runtime::tool_planner::project_tool_policy;
use crate::types::{AgentResult, AgentStatus, Metadata};
use crate::workspace::LocalWorkspaceBackend;

use super::capabilities::{DistributedCapabilityRegistry, ResolvedDistributedCapabilities};
use super::contract::{now_unix_ms, DistributedRunEnvelope};
use super::dispatch::CycleDispatchResult;

#[derive(Clone)]
pub struct DistributedCycleWorker {
    capabilities: DistributedCapabilityRegistry,
}

impl Default for DistributedCycleWorker {
    fn default() -> Self {
        Self::new(DistributedCapabilityRegistry::new())
    }
}

impl DistributedCycleWorker {
    pub fn new(capabilities: DistributedCapabilityRegistry) -> Self {
        Self { capabilities }
    }

    pub fn run_cycle(
        &self,
        envelope: DistributedRunEnvelope,
    ) -> Result<CycleDispatchResult, String> {
        envelope.validate()?;
        envelope.ensure_not_expired()?;
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
        let resolved = self
            .capabilities
            .resolve(&envelope.recipe.capabilities)
            .map_err(|error| error.to_string())?;
        let runtime = build_runtime(&envelope, &resolved)?;
        let now_ms = now_unix_ms()?;
        envelope.ensure_not_expired_at(now_ms)?;
        let lease_expires_at_ms = now_ms
            .checked_add(envelope.lease_duration_ms)
            .ok_or_else(|| "checkpoint lease overflow".to_string())?;
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
        let runtime_result = run_with_lease_heartbeat(
            state_store.clone(),
            &envelope,
            &claim_token,
            checkpoint.revision,
            || runtime.run_with_controls(worker_task, controls),
        )?;
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
        let expected_revision = checkpoint.revision;
        if result.status == AgentStatus::MaxCycles && result.cycles.len() > previous_cycle_count {
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
        Ok(CycleDispatchResult::finished_at_revision(
            result,
            Some(expected_revision + 1),
        ))
    }
}

fn run_with_lease_heartbeat<T>(
    state_store: Arc<dyn StateStore>,
    envelope: &DistributedRunEnvelope,
    claim_token: &str,
    expected_revision: u64,
    operation: impl FnOnce() -> T,
) -> Result<T, String> {
    let stopped = Arc::new((Mutex::new(false), Condvar::new()));
    let heartbeat_error = Arc::new(Mutex::new(None::<String>));
    let interval = Duration::from_millis((envelope.lease_duration_ms / 3).clamp(10, 30_000));
    let task_id = envelope.task.task_id.clone();
    let deadline_unix_ms = envelope.deadline_unix_ms;
    let lease_duration_ms = envelope.lease_duration_ms;
    let claim_token = claim_token.to_string();

    let result = std::thread::scope(|scope| {
        let stopped_for_thread = stopped.clone();
        let error_for_thread = heartbeat_error.clone();
        let heartbeat = scope.spawn(move || loop {
            let (lock, changed) = &*stopped_for_thread;
            let guard = lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let (guard, _) = changed
                .wait_timeout(guard, interval)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if *guard {
                break;
            }
            drop(guard);

            let outcome = (|| {
                let now_ms = now_unix_ms()?;
                if deadline_unix_ms.is_some_and(|deadline| deadline <= now_ms) {
                    return Err(format!(
                        "distributed job deadline expired while renewing {task_id}"
                    ));
                }
                let mut lease_expires_at_ms = now_ms
                    .checked_add(lease_duration_ms)
                    .ok_or_else(|| "checkpoint lease overflow".to_string())?;
                if let Some(deadline) = deadline_unix_ms {
                    lease_expires_at_ms = lease_expires_at_ms.min(deadline);
                }
                let renewed = state_store
                    .renew_checkpoint_claim(
                        &task_id,
                        &claim_token,
                        expected_revision,
                        lease_expires_at_ms,
                        now_ms,
                    )
                    .map_err(|error| error.to_string())?;
                if !renewed {
                    return Err("claim is no longer active".to_string());
                }
                Ok(())
            })();
            if let Err(error) = outcome {
                *error_for_thread
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error);
                break;
            }
        });

        let result = operation();
        let (lock, changed) = &*stopped;
        *lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        changed.notify_all();
        heartbeat
            .join()
            .map_err(|_| "checkpoint lease heartbeat panicked".to_string())?;
        Ok::<_, String>(result)
    })?;

    if let Some(error) = heartbeat_error
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
    {
        return Err(format!("checkpoint lease heartbeat failed: {error}"));
    }
    Ok(result)
}

fn build_runtime(
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
        .with_hooks(resolved.hooks.clone());
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
        initial_messages: Some(checkpoint.messages.clone()),
        initial_shared_state: Some(checkpoint.shared_state.clone()),
        initial_cycles: Some(checkpoint.cycles.clone()),
        cycle_index_start: Some(envelope.cycle_index),
        cycle_count: Some(1),
        ..RuntimeRunControls::default()
    }
}

fn combined_event_handler(
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
        final_answer: None,
        wait_reason: None,
        error: Some(error),
        shared_state,
        token_usage,
    }
}
