use super::*;
use crate::checkpoint::{
    HostInteractionAdmissionContext, HostInteractionOutcome, HostInteractionRequest,
};

impl CheckpointResumeController {
    /// Produce a host interaction only from the currently authoritative
    /// execution claim. Claim identity and its live lease are framework
    /// context; they are reconstructed immediately before the store CAS and
    /// never accepted from request wire data.
    pub(crate) fn produce_host_interaction(
        &mut self,
        request: HostInteractionRequest,
    ) -> CheckpointResult<HostInteractionOutcome> {
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
        let context = HostInteractionAdmissionContext::new(
            &checkpoint.checkpoint_key,
            checkpoint.revision,
            claim_token,
            claimed_cycle,
            now_ms,
            lease_expires_at_ms,
        )?;
        self.store.produce_host_interaction(request, &context)
    }
}
