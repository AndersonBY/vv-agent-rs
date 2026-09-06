use super::*;

impl CheckpointResumeController {
    pub(crate) fn finish_tool(
        &mut self,
        cycle_index: u32,
        call: &ToolCall,
        result: &ToolExecutionResult,
        budget_usage: Option<BudgetUsageSnapshot>,
    ) -> CheckpointResult<Option<AgentResult>> {
        self.reload()?;
        self.set_budget_snapshot(budget_usage);
        let entry = self.find_tool_call(cycle_index, &call.id).ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_journal_integrity_mismatch",
                format!("tool call {:?} is missing from the journal", call.id),
            )
        })?;
        let state = entry.state;
        let receipt = result.clone();
        let journal_tool_call_id = entry.tool_call_id.as_deref().ok_or_else(|| {
            CheckpointError::new(
                "tool_receipt_identity_invalid",
                "tool receipt journal is missing tool_call_id",
            )
        })?;
        if receipt.tool_call_id != journal_tool_call_id {
            return Err(CheckpointError::new(
                "tool_receipt_identity_invalid",
                "tool receipt result tool_call_id does not match the journal",
            ));
        }
        if receipt.status == ToolResultStatus::WaitResponse
            && receipt.directive == crate::types::ToolDirective::WaitUser
        {
            if state == OperationState::Started {
                let event = crate::runtime::state::receipt_event(
                    self.require_checkpoint()?,
                    &entry,
                    &receipt,
                )?;
                self.queue_outbox_entry(event)?;
                self.progress()?;
            }
            return Ok(None);
        }
        if state == OperationState::Planned {
            if receipt.error_code.as_deref() == Some("tool_approval_required") {
                return Ok(None);
            }
            if receipt.status == ToolResultStatus::Success {
                return Ok(None);
            }
            crate::checkpoint::validate_definitive_result(&receipt)?;
            let identity_key = crate::checkpoint::tool_receipt_identity_key(
                &self.require_checkpoint()?.checkpoint_key,
                &entry.operation_id,
                entry.attempt,
                entry.tool_call_id.as_deref().unwrap_or(&call.id),
                &entry.request_digest,
            )?;
            let result_digest = crate::checkpoint::tool_result_digest(&receipt)?;
            let entry = self.find_tool_call_mut(cycle_index, &call.id)?;
            entry.identity_key = Some(identity_key);
            entry.result_digest = Some(result_digest);
            entry.resume_observation = None;
            entry.deferred_handle = None;
            match receipt.status {
                ToolResultStatus::Success => {
                    entry.state = OperationState::Succeeded;
                    entry.result = Some(receipt.to_dict());
                    entry.error = None;
                }
                ToolResultStatus::Error => {
                    entry.state = OperationState::Failed;
                    entry.result = Some(receipt.to_dict());
                    entry.error = Some(crate::runtime::state::operation_error_from_tool_result(
                        &receipt,
                    ));
                    if receipt.error_code.as_deref() == Some("tool_outcome_unknown") {
                        entry.resume_observation =
                            Some(crate::runtime::state::unknown_tool_observation(entry));
                    }
                }
                _ => unreachable!("definitive result validated"),
            }
            entry.validate()?;
            self.progress()?;
            return Ok(None);
        }
        if state != OperationState::Started {
            return Ok(None);
        }

        if crate::checkpoint::is_ambiguous_tool_result(&receipt) {
            let entry = self.find_tool_call_mut(cycle_index, &call.id)?;
            entry.state = OperationState::Ambiguous;
            entry.validate()?;
            self.progress()?;
            let entry = self.find_tool_call(cycle_index, &call.id).ok_or_else(|| {
                CheckpointError::new(
                    "checkpoint_journal_integrity_mismatch",
                    format!("tool call {:?} is missing from the journal", call.id),
                )
            })?;
            return Ok(Some(self.suspend_for(&entry)?));
        }

        if entry.state != OperationState::Started {
            return Ok(None);
        }
        if matches!(
            receipt.status,
            ToolResultStatus::Success | ToolResultStatus::Error
        ) {
            let checkpoint = self.require_checkpoint()?.clone();
            let claim_token = checkpoint.claim_token.as_deref().ok_or_else(|| {
                CheckpointError::new(
                    "checkpoint_claim_active",
                    "checkpoint tool receipt requires an active claim",
                )
            })?;
            let claimed_cycle = checkpoint.claimed_cycle.ok_or_else(|| {
                CheckpointError::new(
                    "checkpoint_claim_active",
                    "checkpoint tool receipt requires an active claimed cycle",
                )
            })?;
            if !self.store.record_tool_receipt(
                checkpoint.clone(),
                &entry.operation_id,
                entry.attempt,
                entry.tool_call_id.as_deref().unwrap_or(&call.id),
                &entry.request_digest,
                receipt,
                claim_token,
                checkpoint.revision,
                claimed_cycle,
            )? {
                return Err(CheckpointError::new(
                    "checkpoint_store_conflict",
                    "checkpoint tool receipt lost its claim",
                ));
            }
            self.checkpoint = Some(
                self.store
                    .load_checkpoint(&checkpoint.checkpoint_key)?
                    .ok_or_else(|| {
                        CheckpointError::new(
                            "checkpoint_not_found",
                            "checkpoint disappeared after tool receipt",
                        )
                    })?,
            );
            return Ok(None);
        }

        let entry = self.find_tool_call_mut(cycle_index, &call.id)?;
        entry.state = OperationState::Ambiguous;
        entry.validate()?;
        self.progress()?;
        let entry = self.find_tool_call(cycle_index, &call.id).ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_journal_integrity_mismatch",
                format!("tool call {:?} is missing from the journal", call.id),
            )
        })?;
        Ok(Some(self.suspend_for(&entry)?))
    }
}
