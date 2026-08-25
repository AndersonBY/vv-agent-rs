macro_rules! memory_impl_core {
() => {
fn store_identity(&self) -> String {
        format!("memory:{:p}", Arc::as_ptr(&self.checkpoints))
}

    fn create_checkpoint(&self, checkpoint: Checkpoint) -> CheckpointResult<bool> {
        checkpoint.validate()?;
        let mut checkpoints = self.lock()?;
        if checkpoints.contains_key(&checkpoint.checkpoint_key) {
            return Ok(false);
        }
        checkpoints.insert(checkpoint.checkpoint_key.clone(), checkpoint);
        Ok(true)
    }

    fn load_checkpoint(&self, checkpoint_key: &str) -> CheckpointResult<Option<Checkpoint>> {
        let checkpoints = self.lock()?;
        Ok(checkpoints.get(checkpoint_key).cloned())
    }

    fn claim_checkpoint(
        &self,
        checkpoint_key: &str,
        cycle_index: u64,
        claim_token: &str,
        lease_expires_at_ms: u64,
        now_ms: u64,
        claim_mode: ClaimMode,
    ) -> CheckpointResult<Option<Checkpoint>> {
        if claim_token.trim().is_empty() || lease_expires_at_ms <= now_ms {
            return Err(CheckpointError::new(
                "checkpoint_claim_invalid",
                "claim token must be non-empty and lease must be in the future",
            ));
        }
        let mut checkpoints = self.lock()?;
        let Some(current) = checkpoints.get(checkpoint_key).cloned() else {
            return Ok(None);
        };
        if !claim_candidate(&current, cycle_index, now_ms, claim_mode)? {
            return Ok(None);
        }
        let mut claimed = current;
        apply_claim(
            &mut claimed,
            cycle_index,
            claim_token,
            lease_expires_at_ms,
            claim_mode,
        )?;
        claimed.validate()?;
        checkpoints.insert(checkpoint_key.to_string(), claimed.clone());
        Ok(Some(claimed))
    }

    fn progress_checkpoint(
        &self,
        checkpoint: Checkpoint,
        claim_token: &str,
        expected_revision: u64,
    ) -> CheckpointResult<bool> {
        let mut checkpoints = self.lock()?;
        let Some(current) = checkpoints.get(&checkpoint.checkpoint_key).cloned() else {
            return Ok(false);
        };
        let Some(updated) = prepare_progress(&current, checkpoint, claim_token, expected_revision)?
        else {
            return Ok(false);
        };
        checkpoints.insert(updated.checkpoint_key.clone(), updated);
        Ok(true)
    }

    fn suspend_checkpoint(
        &self,
        checkpoint: Checkpoint,
        claim_token: &str,
        expected_revision: u64,
    ) -> CheckpointResult<bool> {
        let mut checkpoints = self.lock()?;
        let Some(current) = checkpoints.get(&checkpoint.checkpoint_key).cloned() else {
            return Ok(false);
        };
        let Some(updated) = prepare_suspend(&current, checkpoint, claim_token, expected_revision)?
        else {
            return Ok(false);
        };
        checkpoints.insert(updated.checkpoint_key.clone(), updated);
        Ok(true)
    }

    fn commit_checkpoint(
        &self,
        checkpoint: Checkpoint,
        claim_token: &str,
        expected_revision: u64,
    ) -> CheckpointResult<bool> {
        let mut checkpoints = self.lock()?;
        let Some(current) = checkpoints.get(&checkpoint.checkpoint_key).cloned() else {
            return Ok(false);
        };
        let Some(updated) = prepare_commit(&current, checkpoint, claim_token, expected_revision)?
        else {
            return Ok(false);
        };
        checkpoints.insert(updated.checkpoint_key.clone(), updated);
        Ok(true)
    }

    fn finalize_checkpoint(
        &self,
        checkpoint: Checkpoint,
        expected_revision: u64,
    ) -> CheckpointResult<bool> {
        let mut checkpoints = self.lock()?;
        let Some(current) = checkpoints.get(&checkpoint.checkpoint_key).cloned() else {
            return Ok(false);
        };
        let Some(updated) = prepare_finalize(&current, checkpoint, expected_revision)? else {
            return Ok(false);
        };
        checkpoints.insert(updated.checkpoint_key.clone(), updated);
        Ok(true)
    }

    fn finalize_claimed_checkpoint(
        &self,
        checkpoint: Checkpoint,
        claim_token: &str,
        expected_revision: u64,
    ) -> CheckpointResult<bool> {
        let mut checkpoints = self.lock()?;
        let Some(current) = checkpoints.get(&checkpoint.checkpoint_key).cloned() else {
            return Ok(false);
        };
        let Some(updated) =
            prepare_finalize_claimed(&current, checkpoint, claim_token, expected_revision)?
        else {
            return Ok(false);
        };
        checkpoints.insert(updated.checkpoint_key.clone(), updated);
        Ok(true)
    }

    fn renew_checkpoint_claim(
        &self,
        checkpoint_key: &str,
        claim_token: &str,
        lease_expires_at_ms: u64,
        now_ms: u64,
    ) -> CheckpointResult<bool> {
        if claim_token.trim().is_empty() || lease_expires_at_ms <= now_ms {
            return Err(CheckpointError::new(
                "checkpoint_claim_invalid",
                "claim token must be non-empty and lease must be in the future",
            ));
        }
        let mut checkpoints = self.lock()?;
        let Some(checkpoint) = checkpoints.get_mut(checkpoint_key) else {
            return Ok(false);
        };
        if checkpoint.claim_token.as_deref() != Some(claim_token)
            || checkpoint
                .lease_expires_at_ms
                .is_none_or(|expiry| expiry <= now_ms)
        {
            return Ok(false);
        }
        checkpoint.lease_expires_at_ms = Some(lease_expires_at_ms);
        Ok(true)
    }

    fn acknowledge_terminal(
        &self,
        checkpoint_key: &str,
        expected_revision: u64,
    ) -> CheckpointResult<bool> {
        let mut checkpoints = self.lock()?;
        let Some(current) = checkpoints.get(checkpoint_key).cloned() else {
            return Ok(false);
        };
        let Some(updated) = prepare_ack(&current, expected_revision)? else {
            return Ok(false);
        };
        checkpoints.insert(checkpoint_key.to_string(), updated);
        Ok(true)
    }

    fn record_event_delivery(
        &self,
        checkpoint_key: &str,
        claim_token: Option<&str>,
        expected_revision: u64,
        event_id: &str,
        payload_digest: &str,
        cursor: EventCursor,
    ) -> CheckpointResult<bool> {
        let mut checkpoints = self.lock()?;
        let Some(current) = checkpoints.get(checkpoint_key).cloned() else {
            return Ok(false);
        };
        let Some(updated) = prepare_event_delivery(
            &current,
            claim_token,
            expected_revision,
            event_id,
            payload_digest,
            cursor,
        )?
        else {
            return Ok(false);
        };
        checkpoints.insert(checkpoint_key.to_string(), updated);
        Ok(true)
    }

    fn list_checkpoints(&self) -> CheckpointResult<Vec<String>> {
        Ok(self.lock()?.keys().cloned().collect())
    }

    fn admit_deferred_batch(
        &self,
        checkpoint_key: &str,
        expected_revision: u64,
        claim_token: &str,
        claimed_cycle: u64,
        entries: &[crate::checkpoint::DeferredBatchEntry],
    ) -> CheckpointResult<crate::checkpoint::DeferredBatchAdmission> {
        let mut checkpoints = self.lock()?;
        let current = checkpoints.get(checkpoint_key).cloned().ok_or_else(|| {
            CheckpointError::new("checkpoint_not_found", "checkpoint does not exist")
        })?;
        let (updated, handles) = crate::runtime::state::admit_deferred_batch(
            &current,
            expected_revision,
            claim_token,
            claimed_cycle,
            entries,
        )?;
        if updated.revision == current.revision {
            return Err(CheckpointError::new(
                "checkpoint_revision_conflict",
                "deferred batch admission precondition failed",
            ));
        }
        checkpoints.insert(checkpoint_key.to_string(), updated.clone());
        Ok(crate::checkpoint::DeferredBatchAdmission {
            checkpoint: updated,
            handles,
        })
    }

    fn resolve_deferred(
        &self,
        handle: crate::checkpoint::DeferredToolHandle,
        result: crate::types::ToolExecutionResult,
    ) -> CheckpointResult<crate::checkpoint::DeferredResolveDecision> {
        use crate::checkpoint::{DeferredReceipt, DeferredResolveDecision};
        crate::checkpoint::validate_definitive_result(&result)?;
        let key = handle.handle_key()?;
        let mut checkpoints = self.lock()?;
        let mut receipts = self.receipt_lock()?;
        if let Some(receipt) = receipts.get(&key) {
            if crate::checkpoint::result_digest(&receipt.result)?
                == crate::checkpoint::result_digest(&result)?
            {
                return Ok(DeferredResolveDecision::Replayed {
                    receipt: receipt.clone(),
                });
            }
            return Err(CheckpointError::new(
                "deferred_resolution_conflict",
                "deferred handle already has a different definitive result",
            ));
        }
        let checkpoint = checkpoints
            .get(&handle.checkpoint_key)
            .cloned()
            .ok_or_else(|| {
                CheckpointError::new("deferred_resolution_stale", "checkpoint does not exist")
            })?;
        let Some(index) = checkpoint.tool_journal.iter().position(|entry| {
            entry.operation_id == handle.operation_id
                && entry.attempt == handle.attempt
                && entry.request_digest == handle.request_digest
        }) else {
            return Err(CheckpointError::new(
                "deferred_resolution_stale",
                "no active journal matches the deferred handle",
            ));
        };
        let entry = &checkpoint.tool_journal[index];
        if entry.state == crate::checkpoint::OperationState::Started {
            return Ok(DeferredResolveDecision::not_admitted());
        }
        if entry.state == crate::checkpoint::OperationState::Ambiguous {
            return Ok(DeferredResolveDecision::ReconciliationRequired);
        }
        if checkpoint.claim_token.is_some() {
            return Err(CheckpointError::new(
                "deferred_checkpoint_claimed",
                "deferred resolution is blocked while the checkpoint is claimed",
            ));
        }
        if entry.state != crate::checkpoint::OperationState::Deferred
            || entry.deferred_handle.as_ref() != Some(&handle)
        {
            return Err(CheckpointError::new(
                "deferred_resolution_stale",
                "deferred handle is stale or no longer active",
            ));
        }
        if entry.tool_call_id.as_deref() != Some(result.tool_call_id.as_str()) {
            return Err(CheckpointError::new(
                "deferred_resolution_stale",
                "deferred result tool_call_id does not match the journal",
            ));
        }
        let event = crate::runtime::state::receipt_event(&checkpoint, entry, &result)?;
        let event_id = event.event_id.clone();
        let event_digest = event.payload_digest.clone();
        let mut updated = checkpoint.clone();
        let journal = &mut updated.tool_journal[index];
        match result.status {
            crate::types::ToolResultStatus::Success => {
                journal.state = crate::checkpoint::OperationState::Succeeded;
                journal.result = Some(result.to_dict());
            }
            crate::types::ToolResultStatus::Error => {
                journal.state = crate::checkpoint::OperationState::Failed;
                journal.error = Some(crate::runtime::state::OperationError::new(
                    result
                        .error_code
                        .clone()
                        .unwrap_or_else(|| "tool_error".to_string()),
                    result.content.clone(),
                    false,
                ));
            }
            _ => unreachable!(),
        }
        journal.deferred_handle = None;
        updated.event_outbox.push(event);
        let unresolved = updated
            .tool_journal
            .iter()
            .any(|entry| entry.state == crate::checkpoint::OperationState::Deferred);
        updated.status = if unresolved {
            crate::checkpoint::CheckpointStatus::Deferred
        } else {
            crate::checkpoint::CheckpointStatus::Running
        };
        updated.revision = checkpoint.revision.checked_add(1).ok_or_else(|| {
            CheckpointError::new("checkpoint_revision_overflow", "revision overflow")
        })?;
        updated.validate()?;
        let receipt = DeferredReceipt::new(handle, result, event_id, event_digest)?;
        receipts.insert(key, receipt.clone());
        checkpoints.insert(updated.checkpoint_key.clone(), updated);
        Ok(if unresolved {
            DeferredResolveDecision::AppliedWaiting { receipt }
        } else {
            DeferredResolveDecision::AppliedReady { receipt }
        })
    }

    fn accept_deferred_batch(
        &self,
        checkpoint_key: &str,
        expected_revision: u64,
        claim_token: &str,
        claimed_cycle: u64,
        decisions: &[crate::checkpoint::AcceptDeferredDecision],
    ) -> CheckpointResult<crate::checkpoint::DeferredBatchAdmission> {
        let mut checkpoints = self.lock()?;
        let current = checkpoints.get(checkpoint_key).cloned().ok_or_else(|| {
            CheckpointError::new("checkpoint_not_found", "checkpoint does not exist")
        })?;
        if crate::runtime::state::deferred_batch_is_idempotent(&current, decisions) {
            return Ok(crate::checkpoint::DeferredBatchAdmission {
                checkpoint: current,
                handles: decisions
                    .iter()
                    .map(|decision| decision.handle.clone())
                    .collect(),
            });
        }
        let (updated, changed) = crate::runtime::state::accept_deferred_batch(
            &current,
            expected_revision,
            claim_token,
            claimed_cycle,
            decisions,
        )?;
        if !changed {
            return Err(CheckpointError::new(
                "reconciliation_required",
                "deferred reconciliation precondition failed",
            ));
        }
        let handles = decisions.iter().map(|d| d.handle.clone()).collect();
        checkpoints.insert(checkpoint_key.to_string(), updated.clone());
        Ok(crate::checkpoint::DeferredBatchAdmission {
            checkpoint: updated,
            handles,
        })
    }
};
}
