use super::*;
use crate::checkpoint::{HostInteractionAdmissionContext, HostInteractionRequest};

impl CheckpointResumeController {
    fn host_interaction_admission_context(
        &mut self,
    ) -> CheckpointResult<HostInteractionAdmissionContext> {
        self.assert_heartbeat()?;
        let checkpoint = self.refresh_authoritative()?;
        let claim_token = checkpoint.claim_token.clone().ok_or_else(|| {
            CheckpointError::new(
                "host_interaction_claim_required",
                "host interaction admission requires the active execution claim",
            )
        })?;
        if self.owned_claim_token.as_deref() != Some(claim_token.as_str()) {
            return Err(CheckpointError::new(
                "host_interaction_claim_required",
                "host interaction admission claim is not owned by this execution",
            ));
        }
        let claimed_cycle = checkpoint.claimed_cycle.ok_or_else(|| {
            CheckpointError::new(
                "host_interaction_claim_required",
                "host interaction admission requires the claimed cycle",
            )
        })?;
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| CheckpointError::new("checkpoint_clock_invalid", error.to_string()))?
            .as_millis();
        let now_ms = u64::try_from(now_ms).map_err(|_| {
            CheckpointError::new(
                "checkpoint_clock_invalid",
                "system time is outside the checkpoint integer range",
            )
        })?;
        let lease_expires_at_ms = checkpoint.lease_expires_at_ms.ok_or_else(|| {
            CheckpointError::new(
                "host_interaction_claim_required",
                "host interaction admission requires a live claim lease",
            )
        })?;
        HostInteractionAdmissionContext::new(
            &checkpoint.checkpoint_key,
            checkpoint.revision,
            claim_token,
            claimed_cycle,
            now_ms,
            lease_expires_at_ms,
        )
    }

    pub(crate) fn finish_host_interaction(
        &mut self,
        request: HostInteractionRequest,
        messages: &[Message],
        cycles: &[CycleRecord],
        shared_state: &Metadata,
        budget_usage: Option<BudgetUsageSnapshot>,
    ) -> CheckpointResult<AgentResult> {
        let admission = (|| {
            let mut context = self.host_interaction_admission_context()?;
            self.refresh_snapshot(messages, cycles, shared_state, budget_usage.clone())?;
            context.cycle_snapshot = Some(Box::new(self.require_checkpoint()?.clone()));
            self.store
                .produce_host_interaction(request.clone(), &context)
        })();
        if let Err(error) = admission {
            if error.code() == "checkpoint_cancel_requested" {
                if let Some(cycle) = cycles
                    .iter()
                    .find(|cycle| u64::from(cycle.index) == request.logical_cycle)
                {
                    if let Some((call, result)) = cycle
                        .tool_calls
                        .iter()
                        .zip(&cycle.tool_results)
                        .find(|(call, _)| call.id == request.tool_call_id)
                    {
                        self.finish_tool(cycle.index, call, result, budget_usage)?;
                    }
                }
            }
            return Err(error);
        }
        self.owned_claim_token = None;
        self.first_claim_is_recovery = false;
        self.stop_heartbeat();
        self.reload()?;
        self.deliver_pending_outbox()?;
        let checkpoint = self.require_checkpoint()?;
        Ok(AgentResult {
            status: AgentStatus::HostInteraction,
            messages: messages.to_vec(),
            cycles: cycles.to_vec(),
            completion_reason: None,
            completion_tool_name: None,
            partial_output: last_assistant_output(cycles),
            budget_usage: checkpoint.budget_usage.clone(),
            budget_exhaustion: None,
            checkpoint_key: Some(checkpoint.checkpoint_key.clone()),
            resume_observations: Vec::new(),
            final_answer: None,
            wait_reason: Some("host_interaction".to_string()),
            error: None,
            error_code: None,
            shared_state: shared_state.clone(),
            token_usage: summarize_task_token_usage(&checkpoint.model_calls),
        })
    }
}
