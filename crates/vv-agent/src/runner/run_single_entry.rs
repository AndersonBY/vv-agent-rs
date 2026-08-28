//! Standard single-run entrypoint layered over the distributed-aware operation.

use super::*;

pub(super) fn distributed_compiled_initial_messages(
    operation: Option<&DistributedRunnerOperation>,
) -> Option<(AgentTask, Vec<crate::types::Message>)> {
    operation.and_then(|operation| match operation {
        DistributedRunnerOperation::StartCompiled(task) => {
            Some((task.clone(), task.initial_messages.clone()))
        }
        _ => None,
    })
}

pub(super) fn distributed_checkpoint_options<'a>(
    operation: Option<&'a DistributedRunnerOperation>,
    execution_backend: &RuntimeExecutionBackend,
) -> (Option<&'a DistributedAdvanceDecision>, Option<u64>) {
    (
        operation.and_then(|operation| match operation {
            DistributedRunnerOperation::Finalize(decision) => Some(decision.as_ref()),
            _ => None,
        }),
        match execution_backend {
            RuntimeExecutionBackend::Distributed(backend) => Some(backend.lease_duration_ms()),
            _ => None,
        },
    )
}

pub(super) fn result_terminal_flags(result: &AgentResult) -> (bool, bool, bool) {
    let reconciliation_required = result.status == AgentStatus::ReconciliationRequired;
    let operator_abort = result.status == AgentStatus::Failed
        && (result.error_code.as_deref() == Some("operator_abort_with_unknown_outcome")
            || result.error.as_deref() == Some("operator_abort_with_unknown_outcome"))
        && result.resume_observation.is_some();
    let deferred = matches!(result.status, AgentStatus::Deferred);
    (reconciliation_required, operator_abort, deferred)
}

impl Runner {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn run_single_agent(
        &self,
        agent: &Agent,
        input: NormalizedInput,
        config: RunConfig,
        event_collector: Option<Arc<std::sync::Mutex<Vec<RunEvent>>>>,
        event_sender: Option<broadcast::Sender<RunEvent>>,
        checkpoint_admission_sender: Option<CheckpointAdmissionSender>,
        run_id_override: Option<String>,
    ) -> Result<SingleRunOutcome, String> {
        match self.run_single_agent_operation(
            agent,
            input,
            config,
            event_collector,
            event_sender,
            checkpoint_admission_sender,
            run_id_override,
            None,
        )? {
            SingleRunExecutionOutcome::Completed(outcome) => Ok(*outcome),
            SingleRunExecutionOutcome::DistributedStarted(_) => Err(
                "distributed start escaped the dedicated Runner::start_distributed entrypoint"
                    .to_string(),
            ),
        }
    }
}
