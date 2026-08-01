//! Standard single-run entrypoint layered over the distributed-aware operation.

use super::*;

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
