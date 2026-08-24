use super::*;

impl CheckpointResumeController {
    /// Prove the claimed checkpoint can persist lifecycle state before an
    /// external tool effect. The progress CAS is a writability probe; the
    /// contract deliberately has no fixed outbox cardinality cap.
    pub(crate) fn preflight_tool_dispatch(
        &mut self,
        cycle_index: u32,
        call: &ToolCall,
    ) -> CheckpointResult<()> {
        let checkpoint = self.require_checkpoint()?;
        if checkpoint.claim_token.is_none() {
            return Err(CheckpointError::new(
                "checkpoint_claim_active",
                "tool dispatch requires an active claim",
            ));
        }
        let entry = checkpoint
            .tool_journal
            .iter()
            .find(|entry| {
                entry.cycle_index == u64::from(cycle_index)
                    && entry.tool_call_id.as_deref() == Some(call.id.as_str())
            })
            .ok_or_else(|| {
                CheckpointError::new(
                    "checkpoint_journal_integrity_mismatch",
                    format!("tool call {:?} is missing from the journal", call.id),
                )
            })?;
        if entry.state != OperationState::Planned {
            return Ok(());
        }
        checkpoint.validate()?;
        self.progress()
    }
}
