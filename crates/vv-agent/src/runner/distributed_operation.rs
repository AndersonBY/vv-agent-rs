//! Runner-side preparation for enqueue-only distributed execution.

use super::*;

pub(super) fn start_distributed_cycle<C: LlmClient>(
    runtime: &AgentRuntime<C>,
    controller: &CheckpointController,
    task: &AgentTask,
    budget_limits: Option<crate::budget::RunBudgetLimits>,
) -> Result<DistributedRunHandle, String> {
    let distributed_checkpoint_config = {
        let controller = controller.lock().map_err(|_| {
            "checkpoint_store_lock_poisoned: checkpoint controller lock poisoned".to_string()
        })?;
        let checkpoint_config = controller.checkpoint_config();
        DistributedCheckpointConfig {
            key: controller
                .checkpoint_key()
                .map_err(|error| error.to_string())?
                .to_string(),
            resume_policy: ResumePolicy::RequireExisting,
            ambiguous_model_policy: checkpoint_config.ambiguous_model_policy,
            ambiguous_tool_policy: checkpoint_config.ambiguous_tool_policy,
            required_extension_namespaces: checkpoint_config.required_extension_namespaces.clone(),
            max_extension_state_bytes: checkpoint_config.max_extension_state_bytes,
            credential_slots: checkpoint_config.credential_slots.clone(),
        }
    };
    let RuntimeExecutionBackend::Distributed(backend) = &runtime.execution_backend else {
        return Err("nonblocking distributed start requires a DistributedBackend".to_string());
    };
    let started = backend.start(task.clone(), distributed_checkpoint_config, budget_limits);
    controller
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .close();
    started
}

pub(super) fn distributed_checkpoint_resume(
    operation: Option<&DistributedRunnerOperation>,
    checkpoint_exists: bool,
    checkpoint_resume: bool,
) -> Result<bool, String> {
    if !matches!(operation, Some(DistributedRunnerOperation::Finalize(_))) {
        return Ok(checkpoint_resume);
    }
    if !checkpoint_exists {
        return Err(
            "checkpoint_not_found: distributed terminal checkpoint disappeared".to_string(),
        );
    }
    Ok(true)
}
