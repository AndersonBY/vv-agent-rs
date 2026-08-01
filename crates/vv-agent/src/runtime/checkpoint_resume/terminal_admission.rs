//! Admission of terminal candidates produced by distributed cycle workers.

use super::*;

impl CheckpointResumeController {
    pub(crate) fn admit_terminal_candidate(
        &mut self,
        checkpoint_revision: u64,
        requires_claim: bool,
        lease_duration_ms: u64,
    ) -> CheckpointResult<Option<AgentResult>> {
        let checkpoint_key = self
            .config
            .key
            .as_deref()
            .ok_or_else(|| {
                CheckpointError::new(
                    "checkpoint_key_required",
                    "distributed terminal finalization requires an explicit checkpoint key",
                )
            })?
            .to_string();
        let checkpoint = self
            .store
            .load_checkpoint(&checkpoint_key)?
            .ok_or_else(|| {
                CheckpointError::new(
                    "checkpoint_not_found",
                    "checkpoint disappeared before terminal candidate admission",
                )
            })?;
        if checkpoint.task_id != self.task_id
            || checkpoint.root_run_id != self.run_id
            || checkpoint.trace_id != self.trace_id
            || checkpoint.run_definition_digest != self.run_definition_digest
        {
            return Err(CheckpointError::new(
                "checkpoint_store_conflict",
                "terminal candidate does not match the authoritative checkpoint",
            ));
        }
        self.checkpoint = Some(checkpoint);
        if self.require_checkpoint()?.terminal_result.is_some() {
            self.deliver_pending_outbox()?;
            self.acknowledge_terminal()?;
            let mut result = AgentResult::from_dict(
                self.require_checkpoint()?
                    .terminal_result
                    .as_ref()
                    .expect("terminal checked above"),
            )
            .map_err(|error| CheckpointError::new("checkpoint_terminal_result_invalid", error))?;
            result.checkpoint_key = Some(checkpoint_key);
            return Ok(Some(result));
        }
        let checkpoint = self.require_checkpoint()?;
        if checkpoint.revision != checkpoint_revision
            || requires_claim != checkpoint.claim_token.is_some()
        {
            return Err(CheckpointError::new(
                "checkpoint_store_conflict",
                "terminal candidate does not match the authoritative checkpoint",
            ));
        }
        let claim_token = checkpoint.claim_token.clone();
        if let Some(claim_token) = claim_token {
            self.adopt_claim_for_terminal_finalize(&claim_token, lease_duration_ms)?;
        }
        self.restore_extensions()?;
        Ok(None)
    }
}
