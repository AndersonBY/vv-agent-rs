macro_rules! redis_impl_tail {
() => {
    fn accept_deferred_batch(
        &self,
        checkpoint_key: &str,
        expected_revision: u64,
        claim_token: &str,
        claimed_cycle: u64,
        decisions: &[crate::checkpoint::AcceptDeferredDecision],
    ) -> CheckpointResult<crate::checkpoint::DeferredBatchAdmission> {
        let data_key = Self::data_key(checkpoint_key);
        let lease_key = Self::lease_key(checkpoint_key);
        let result = self.transaction(&data_key, &lease_key, |connection, pipeline| {
            let Some(raw) = connection
                .get::<_, Option<String>>(&data_key)
                .map_err(redis_error)?
            else {
                return Err(CheckpointError::new(
                    "checkpoint_not_found",
                    "checkpoint does not exist",
                ));
            };
            let current = decode_storage(
                &raw,
                connection
                    .get::<_, Option<u64>>(&lease_key)
                    .map_err(redis_error)?,
            )?;
            if crate::runtime::state::deferred_batch_is_idempotent(&current, decisions) {
                return Ok(Some(crate::checkpoint::DeferredBatchAdmission {
                    checkpoint: current,
                    handles: decisions
                        .iter()
                        .map(|decision| decision.handle.clone())
                        .collect(),
                }));
            }
            let (updated, changed) = crate::runtime::state::accept_deferred_batch(
                &current,
                expected_revision,
                claim_token,
                claimed_cycle,
                decisions,
            )?;
            if !changed {
                return Ok(None);
            }
            let payload = checkpoint_to_json(&updated, MAX_EXTENSION_STATE_BYTES)?;
            pipeline.set(&data_key, payload).ignore();
            pipeline.del(&lease_key).ignore();
            Ok(Some(crate::checkpoint::DeferredBatchAdmission {
                checkpoint: updated,
                handles: decisions
                    .iter()
                    .map(|decision| decision.handle.clone())
                    .collect(),
            }))
        });
        match result {
            Ok(value) => Ok(value),
            Err(error) if error.code() == "checkpoint_store_conflict" => Err(CheckpointError::new(
                "reconciliation_required",
                "deferred reconciliation compare-and-swap failed",
            )),
            Err(error) => Err(error),
        }
    }

    fn produce_host_interaction(
        &self,
        request: HostInteractionRequest,
        context: &crate::checkpoint::HostInteractionAdmissionContext,
    ) -> CheckpointResult<HostInteractionOutcome> {
        redis_produce_host_interaction(self, request, context)
    }

    fn admit_controller_command(
        &self,
        command: ControllerCommand,
    ) -> CheckpointResult<ControllerCommandReceipt> {
        match self.resolve_controller_command(command)? {
            ControllerCommandResolution::Applied { receipt, .. }
            | ControllerCommandResolution::Replayed { receipt, .. } => Ok(receipt),
            ControllerCommandResolution::Rejected { error } => Err(CheckpointError::new(
                "controller_command_invalid_state",
                error,
            )),
        }
    }

    fn resolve_controller_command(
        &self,
        command: ControllerCommand,
    ) -> CheckpointResult<ControllerCommandResolution> {
        redis_resolve_controller_command(self, command)
    }

    fn get_controller_command_receipt(
        &self,
        command_id: &str,
    ) -> CheckpointResult<Option<ControllerCommandReceipt>> {
        redis_get_controller_command_receipt(self, command_id)
    }

    fn get_controller_command(
        &self,
        command_id: &str,
    ) -> CheckpointResult<Option<ControllerCommand>> {
        redis_get_controller_command(self, command_id)
    }

    fn claim_and_consume_host_interaction_response(
        &self,
        envelope: HostInteractionRecoveryEnvelope,
    ) -> CheckpointResult<HostInteractionRecoveryResult> {
        redis_claim_and_consume_host_interaction_response(self, envelope)
    }

    fn reap_host_interaction_record(
        &self,
        record_id: &str,
        checkpoint_key: &str,
        now_ms: u64,
    ) -> CheckpointResult<bool> {
        redis_reap_host_interaction_record(self, record_id, checkpoint_key, now_ms)
    }

    fn claim_host_interaction_notification(
        &self,
        notification_id: &str,
        payload_digest: &str,
        claim_token: &str,
        lease_expires_at_ms: u64,
        now_ms: u64,
    ) -> CheckpointResult<Option<HostInteractionNotificationRecord>> {
        redis_claim_host_interaction_notification(
            self,
            notification_id,
            payload_digest,
            claim_token,
            lease_expires_at_ms,
            now_ms,
        )
    }

    fn get_host_interaction_notification(
        &self,
        notification_id: &str,
    ) -> CheckpointResult<Option<HostInteractionNotificationRecord>> {
        redis_get_host_interaction_notification(self, notification_id)
    }

    fn complete_host_interaction_notification(
        &self,
        notification_id: &str,
        payload_digest: &str,
        claim_token: &str,
        attempt: u64,
        outcome: &str,
        now_ms: u64,
        error: Option<&str>,
    ) -> CheckpointResult<Option<HostInteractionNotificationRecord>> {
        redis_complete_host_interaction_notification(
            self,
            notification_id,
            payload_digest,
            claim_token,
            attempt,
            outcome,
            now_ms,
            error,
        )
    }

    fn reconcile_host_interaction_notification(
        &self,
        notification_id: &str,
        payload_digest: &str,
        outcome: &str,
        now_ms: u64,
        abort_reason: Option<&str>,
    ) -> CheckpointResult<Option<HostInteractionNotificationRecord>> {
        redis_reconcile_host_interaction_notification(
            self,
            notification_id,
            payload_digest,
            outcome,
            now_ms,
            abort_reason,
        )
    }

    fn claim_controller_command_wake(
        &self,
        command_id: &str,
        command_digest: &str,
        claim_token: &str,
        lease_expires_at_ms: u64,
        now_ms: u64,
    ) -> CheckpointResult<Option<ControllerCommandReceipt>> {
        redis_claim_controller_command_wake(
            self,
            command_id,
            command_digest,
            claim_token,
            lease_expires_at_ms,
            now_ms,
        )
    }

    fn complete_controller_command_wake(
        &self,
        command_id: &str,
        command_digest: &str,
        claim_token: &str,
        attempt: u64,
        outcome: &str,
        now_ms: u64,
        error: Option<&str>,
    ) -> CheckpointResult<Option<ControllerCommandReceipt>> {
        redis_complete_controller_command_wake(
            self,
            command_id,
            command_digest,
            claim_token,
            attempt,
            outcome,
            now_ms,
            error,
        )
    }

    fn reconcile_controller_command_wake(
        &self,
        command_id: &str,
        command_digest: &str,
        outcome: &str,
        now_ms: u64,
    ) -> CheckpointResult<Option<ControllerCommandReceipt>> {
        redis_reconcile_controller_command_wake(self, command_id, command_digest, outcome, now_ms)
    }

    fn reap_controller_command_wake(
        &self,
        command_id: &str,
        command_digest: &str,
        now_ms: u64,
    ) -> CheckpointResult<bool> {
        redis_reap_controller_command_wake(self, command_id, command_digest, now_ms)
    }

    fn delete_checkpoint(&self, checkpoint_key: &str) -> CheckpointResult<()> {
        redis_delete_checkpoint(self, checkpoint_key)
    }

    fn list_checkpoints(&self) -> CheckpointResult<Vec<String>> {
        redis_list_checkpoints(self)
    }

};
}
