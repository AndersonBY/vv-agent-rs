macro_rules! memory_impl_controller {
() => {
fn produce_host_interaction(
        &self,
        request: HostInteractionRequest,
        context: &crate::checkpoint::HostInteractionAdmissionContext,
    ) -> CheckpointResult<HostInteractionOutcome> {
        request.validate()?;
        context.validate()?;
        let context_is_live = context.validate_live_lease().is_ok();
        let mut checkpoints = self.lock()?;
        let mut ledger = self.controller_lock()?;

        // Replay is intentionally checked before claim discovery: admission
        // releases the execution claim, so a transport retry must be able to
        // return the retained outcome from the host record alone.
        if let Some((record_id, existing)) = ledger
            .host_interactions
            .iter()
            .find(|(_, record)| {
                record.checkpoint_key == context.checkpoint_key
                    && record.interaction_id == request.interaction_id
            })
        {
            if existing.request != request {
                return Err(CheckpointError::new(
                    "host_interaction_conflict",
                    "interaction identity is already bound to a different request",
                ));
}
            let checkpoint = checkpoints.get(&existing.checkpoint_key).ok_or_else(|| {
                CheckpointError::new(
                    "host_interaction_conflict",
                    "retained host interaction has no checkpoint",
                )
            })?;
            let notification_id = notification_id_for(record_id);
            let notification = ledger.notifications.get(&notification_id).ok_or_else(|| {
                CheckpointError::new(
                    "host_interaction_conflict",
                    "retained host interaction has no notification outbox row",
                )
            })?;
            return host_interaction_outcome(
                &request,
                checkpoint.revision,
                "replayed",
                record_id,
                notification,
            );
        }

        let checkpoint_key = &context.checkpoint_key;
        let current = checkpoints
            .get(checkpoint_key)
            .cloned()
            .ok_or_else(|| {
                CheckpointError::new(
                    "host_interaction_claim_required",
                    "host interaction admission checkpoint was not found",
                )
            })?;
        if current.status != crate::checkpoint::CheckpointStatus::Running
            || current.revision != context.expected_revision
            || current.claim_token.as_deref() != Some(context.claim_token.as_str())
            || current.claimed_cycle != Some(context.claimed_cycle)
            || request.logical_cycle != context.claimed_cycle
            || current.lease_expires_at_ms != Some(context.lease_expires_at_ms)
            || !context_is_live
        {
            return Err(CheckpointError::new(
                "host_interaction_claim_required",
                "host interaction admission claim is stale or expired",
            ));
        }
        let record_id = record_id_for(checkpoint_key, &request);
        if let Some(existing) = ledger.host_interactions.get(&record_id) {
            if existing.request == request
                && existing.checkpoint_key == current.checkpoint_key
                && existing.logical_cycle == request.logical_cycle
            {
                let notification_id = notification_id_for(&record_id);
                let notification = ledger.notifications.get(&notification_id).ok_or_else(|| {
                    CheckpointError::new(
                        "host_interaction_conflict",
                        "retained host interaction has no notification outbox row",
                    )
                })?;
                let outcome = host_interaction_outcome(
                    &request,
                    current.revision,
                    "replayed",
                    &record_id,
                    notification,
                )?;
                return Ok(outcome);
            }
            return Err(CheckpointError::new(
                "host_interaction_conflict",
                "interaction identity is already bound to a different request",
            ));
        }
        let notification_id = notification_id_for(&record_id);
        let notification_payload = HostInteractionNotificationPayload {
            schema_version: HOST_INTERACTION_NOTIFICATION_SCHEMA.to_string(),
            notification_id: notification_id.clone(),
            record_id: record_id.clone(),
            interaction_id: request.interaction_id.clone(),
            logical_cycle: request.logical_cycle,
            status: "host_interaction".to_string(),
            wait_reason: "host_interaction".to_string(),
            prompt: sanitize_public_prompt(&request.prompt),
        };
        notification_payload.validate()?;
        let notification = HostInteractionNotificationRecord {
            notification_id: notification_id.clone(),
            checkpoint_key: checkpoint_key.clone(),
            record_id: record_id.clone(),
            payload_digest: notification_payload.digest()?,
            payload: notification_payload.clone(),
            outbox_state: NotificationOutboxState::Pending,
            claim_token: None,
            lease_expires_at_ms: None,
            attempt: 0,
            delivered_at_ms: None,
            aborted_at_ms: None,
            abort_reason: None,
            last_error: None,
        };
        notification.validate()?;
        let record = HostInteractionRecord {
            schema_version: HOST_INTERACTION_RECORD_SCHEMA.to_string(),
            record_id: record_id.clone(),
            checkpoint_key: checkpoint_key.clone(),
            interaction_id: request.interaction_id.clone(),
            logical_cycle: request.logical_cycle,
            attempt: 0,
            claim_token: None,
            lease_expires_at_ms: None,
            request: request.clone(),
            request_digest: request.request_digest.clone(),
            state: "active".to_string(),
            response: None,
            response_digest: None,
            command_id: None,
            resolved_revision: None,
            consumed_revision: None,
            last_error: None,
        };
        record.validate()?;

        let cycle_index = u32::try_from(request.logical_cycle).map_err(|_| {
            CheckpointError::new(
                "host_interaction_cycle_invalid",
                "logical cycle does not fit the RunEvent cycle index",
            )
        })?;
        let mut event = RunEvent::new(
            current.root_run_id.clone(),
            current.trace_id.clone(),
            "vv-agent",
            Some(cycle_index.saturating_sub(1)),
            RunEventPayload::HostInteractionRequested {
                checkpoint_key: current.checkpoint_key.clone(),
                resume_attempt: current.resume_attempt,
                interaction_id: request.interaction_id.clone(),
                logical_cycle: request.logical_cycle,
                operation_id: request.operation_id.clone(),
                tool_call_id: request.tool_call_id.clone(),
                request_digest: request.request_digest.clone(),
                prompt: notification_payload.prompt.clone(),
            },
        );
        event.event_id = EventId::stable(format!("host-interaction-requested-{record_id}"))
            .map_err(|error| CheckpointError::new("event_identity_conflict", error))?;
        let event_value = serde_json::to_value(&event)
            .map_err(|error| CheckpointError::new("checkpoint_event_invalid", error.to_string()))?;
        let mut updated = current.clone();
        updated.status = crate::checkpoint::CheckpointStatus::HostInteraction;
        updated.active_host_interaction = Some(request.clone());
        updated.claim_token = None;
        updated.claimed_cycle = None;
        updated.lease_expires_at_ms = None;
        updated.revision = current.revision.checked_add(1).ok_or_else(|| {
            CheckpointError::new("checkpoint_revision_overflow", "revision overflow")
        })?;
        updated
            .event_outbox
            .push(crate::runtime::state::EventOutboxEntry::pending(
                event.event_id.as_str(),
                event_value,
            )?);
        updated.validate()?;

        // The three durable rows and the claim release are one in-memory
        // critical section.  Other stores use the same ordering in SQL.
        let outcome = host_interaction_outcome(
            &request,
            updated.revision,
            "admitted",
            &record_id,
            &notification,
        )?;
        checkpoints.insert(checkpoint_key.clone(), updated);
        ledger.host_interactions.insert(record_id, record);
        ledger.notifications.insert(notification_id, notification);
        Ok(outcome)
    }

    fn admit_controller_command(
        &self,
        command: ControllerCommand,
    ) -> CheckpointResult<ControllerCommandReceipt> {
        command.validate()?;
        let mut checkpoints = self.lock()?;
        let mut ledger = self.controller_lock()?;
        if let Some((receipt, _)) = ledger.command_receipts.get(&command.command_id) {
            if receipt.command_digest == command.command_digest {
                return Ok(receipt.clone());
            }
            return Err(CheckpointError::new(
                "controller_command_conflict",
                "command_id is already bound to a different command digest",
            ));
        }
        let (receipt, resolution) =
            apply_controller_command(&mut checkpoints, &mut ledger, &command)?;
        ledger
            .command_receipts
            .insert(command.command_id.clone(), (receipt.clone(), resolution));
        ledger.commands.insert(command.command_id.clone(), command);
        Ok(receipt)
    }

    fn resolve_controller_command(
        &self,
        command: ControllerCommand,
    ) -> CheckpointResult<ControllerCommandResolution> {
        command.validate()?;
        let mut checkpoints = self.lock()?;
        let mut ledger = self.controller_lock()?;
        if let Some((receipt, resolution)) = ledger.command_receipts.get(&command.command_id) {
            if receipt.command_digest == command.command_digest {
                return Ok(replay_resolution(resolution));
            }
            return Err(CheckpointError::new(
                "controller_command_conflict",
                "command_id is already bound to a different command digest",
            ));
        }
        let (receipt, resolution) = match apply_controller_command(
            &mut checkpoints,
            &mut ledger,
            &command,
        ) {
            Ok(result) => result,
            Err(error)
                if matches!(
                    error.code(),
                    "controller_command_stale" | "controller_command_terminal"
                ) => {
                    return Ok(ControllerCommandResolution::Rejected {
                        error: error.to_string(),
                    });
                }
            Err(error) => return Err(error),
        };
        ledger
            .command_receipts
            .insert(command.command_id.clone(), (receipt, resolution.clone()));
        ledger.commands.insert(command.command_id.clone(), command);
        Ok(resolution)
    }

    fn get_controller_command_receipt(
        &self,
        command_id: &str,
    ) -> CheckpointResult<Option<ControllerCommandReceipt>> {
        let ledger = self.controller_lock()?;
        Ok(ledger
            .command_receipts
            .get(command_id)
            .map(|(receipt, _)| receipt.clone()))
    }

    fn get_controller_command(
        &self,
        command_id: &str,
    ) -> CheckpointResult<Option<ControllerCommand>> {
        let ledger = self.controller_lock()?;
        Ok(ledger.commands.get(command_id).cloned())
    }

    fn claim_and_consume_host_interaction_response(
        &self,
        envelope: HostInteractionRecoveryEnvelope,
    ) -> CheckpointResult<HostInteractionRecoveryResult> {
        envelope.validate()?;
        let mut checkpoints = self.lock()?;
        let mut ledger = self.controller_lock()?;
        let current = checkpoints
            .get(&envelope.checkpoint_key)
            .cloned()
            .ok_or_else(|| {
                CheckpointError::new(
                    "host_interaction_recovery_stale",
                    "checkpoint does not exist",
                )
            })?;
        let record = ledger
            .host_interactions
            .get(&envelope.record_id)
            .cloned()
            .ok_or_else(|| {
                CheckpointError::new(
                    "host_interaction_recovery_stale",
                    "host interaction record does not exist",
                )
            })?;
        if record.state == "consumed" {
            if !recovery_identity_matches(&current, &record, &envelope) {
                return Err(CheckpointError::new(
                    "host_interaction_recovery_stale",
                    "recovery envelope does not match consumed record",
                ));
            }
            let result = HostInteractionRecoveryResult {
                schema_version: crate::checkpoint::HOST_INTERACTION_RECOVERY_RESULT_SCHEMA
                    .to_string(),
                kind: "replayed".to_string(),
                record_id: record.record_id,
                checkpoint_revision: Some(current.revision),
                consumed_revision: record.consumed_revision,
                claim_mode: "recovery".to_string(),
                resume_attempt: Some(current.resume_attempt),
                injection_count: 1,
                checkpoint_execution_claim_state: if current.claim_token.is_some() {
                    "retained"
                } else {
                    "released"
                }
                .to_string(),
                error: None,
            };
            result.validate()?;
            return Ok(result);
        }
        if !recovery_identity_matches(&current, &record, &envelope)
            || current.revision != envelope.expected_revision
            || current.resume_attempt != envelope.resume_attempt
            || current.status != crate::checkpoint::CheckpointStatus::Running
            || current.claim_token.is_some()
            || current.has_ambiguous_operation()
            || record.state != "resolved_pending"
        {
            return Err(CheckpointError::new(
                "host_interaction_recovery_stale",
                "recovery envelope is stale or the hard recovery barrier is not admissible",
            ));
        }
        let response = record.response.clone().ok_or_else(|| {
            CheckpointError::new(
                "host_interaction_recovery_stale",
                "resolved record has no response",
            )
        })?;
        let claim_token = format!(
            "host-recovery:{}:{}",
            record.record_id,
            envelope.resume_attempt + 1
        );
        let claimed_cycle = current.cycle_index.checked_add(1).ok_or_else(|| {
            CheckpointError::new("checkpoint_cycle_invalid", "cycle index overflow")
        })?;
        if claimed_cycle != envelope.logical_cycle {
            return Err(CheckpointError::new(
                "host_interaction_recovery_stale",
                "logical cycle does not match checkpoint",
            ));
        }
        let mut updated = current.clone();
        updated.messages.push(crate::types::Message::user(
            response.response.content.clone(),
        ));
        updated.resume_attempt = envelope.resume_attempt.checked_add(1).ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_resume_attempt_overflow",
                "resume attempt overflow",
            )
        })?;
        updated.claim_token = Some(claim_token);
        updated.claimed_cycle = Some(claimed_cycle);
        updated.lease_expires_at_ms = Some(recovery_lease_deadline());
        updated.revision = envelope.expected_revision.checked_add(1).ok_or_else(|| {
            CheckpointError::new("checkpoint_revision_overflow", "revision overflow")
        })?;
        let cycle_index = u32::try_from(envelope.logical_cycle).map_err(|_| {
            CheckpointError::new(
                "host_interaction_recovery_stale",
                "logical cycle does not fit event",
            )
        })?;
        let mut event = RunEvent::new(
            current.root_run_id.clone(),
            current.trace_id.clone(),
            "vv-agent",
            Some(cycle_index.saturating_sub(1)),
            RunEventPayload::HostInteractionResponseConsumed {
                checkpoint_key: current.checkpoint_key.clone(),
                resume_attempt: updated.resume_attempt,
                interaction_id: record.interaction_id.clone(),
                logical_cycle: record.logical_cycle,
                operation_id: record.request.operation_id.clone(),
                tool_call_id: record.request.tool_call_id.clone(),
                request_digest: record.request_digest.clone(),
                command_id: response.command_id.clone(),
                response_digest: response.response_digest.clone(),
                consumed_revision: updated.revision,
            },
        );
        event.event_id = EventId::stable(format!(
            "host-interaction-response-consumed-{}",
            record.record_id
        ))
        .map_err(|error| CheckpointError::new("event_identity_conflict", error))?;
        let event_value = serde_json::to_value(&event)
            .map_err(|error| CheckpointError::new("checkpoint_event_invalid", error.to_string()))?;
        updated
            .event_outbox
            .push(crate::runtime::state::EventOutboxEntry::pending(
                event.event_id.as_str(),
                event_value,
            )?);
        updated.validate()?;
        let mut consumed = record;
        consumed.state = "consumed".to_string();
        consumed.consumed_revision = Some(updated.revision);
        consumed.claim_token = None;
        consumed.lease_expires_at_ms = None;
        consumed.validate()?;
        checkpoints.insert(envelope.checkpoint_key, updated.clone());
        ledger
            .host_interactions
            .insert(consumed.record_id.clone(), consumed.clone());
        let result = HostInteractionRecoveryResult {
            schema_version: crate::checkpoint::HOST_INTERACTION_RECOVERY_RESULT_SCHEMA.to_string(),
            kind: "applied".to_string(),
            record_id: consumed.record_id,
            checkpoint_revision: Some(updated.revision),
            consumed_revision: consumed.consumed_revision,
            claim_mode: "recovery".to_string(),
            resume_attempt: Some(updated.resume_attempt),
            injection_count: 1,
            checkpoint_execution_claim_state: "retained".to_string(),
            error: None,
        };
        result.validate()?;
        Ok(result)
    }

    fn reap_host_interaction_record(
        &self,
        record_id: &str,
        checkpoint_key: &str,
        now_ms: u64,
    ) -> CheckpointResult<bool> {
        let checkpoints = self.lock()?;
        let mut ledger = self.controller_lock()?;
        let Some(record) = ledger.host_interactions.get(record_id).cloned() else {
            return Ok(false);
        };
        if record.checkpoint_key != checkpoint_key
            || record.state != "resolved_claimed"
            || record.claim_token.is_none()
        {
            return Ok(false);
        }
        let Some(checkpoint) = checkpoints.get(checkpoint_key) else {
            return Ok(false);
        };
        if checkpoint.status != crate::checkpoint::CheckpointStatus::Running
            || checkpoint.claim_token.as_deref() != record.claim_token.as_deref()
            || checkpoint.claim_token.is_none()
            || checkpoint
                .lease_expires_at_ms
                .is_none_or(|lease| lease > now_ms)
            || record
                .lease_expires_at_ms
                .is_none_or(|lease| lease > now_ms)
        {
            return Ok(false);
        }
        let mut updated = record;
        updated.state = "resolved_pending".to_string();
        updated.claim_token = None;
        updated.lease_expires_at_ms = None;
        updated.last_error = Some("host_interaction_response_claim_expired".to_string());
        updated.validate()?;
        ledger.host_interactions.insert(record_id.to_string(), updated);
        Ok(true)
    }

    fn claim_host_interaction_notification(
        &self,
        notification_id: &str,
        payload_digest: &str,
        claim_token: &str,
        lease_expires_at_ms: u64,
        now_ms: u64,
    ) -> CheckpointResult<Option<HostInteractionNotificationRecord>> {
        if claim_token.trim().is_empty() || lease_expires_at_ms <= now_ms {
            return Err(CheckpointError::new(
                "notification_conflict",
                "notification claim token must be non-empty and lease must be in the future",
            ));
        }
        let mut ledger = self.controller_lock()?;
        let Some(row) = ledger.notifications.get(notification_id).cloned() else {
            return Ok(None);
        };
        if row.payload_digest != payload_digest {
            return Err(CheckpointError::new(
                "notification_conflict",
                "notification payload digest conflicts",
            ));
        }
        if matches!(
            row.outbox_state,
            NotificationOutboxState::Delivered | NotificationOutboxState::Aborted
        ) {
            return Ok(Some(row));
        }
        if row.outbox_state == NotificationOutboxState::Claimed
            && row.claim_token.as_deref() != Some(claim_token)
            && row.lease_expires_at_ms.is_some_and(|lease| lease > now_ms)
        {
            return Err(CheckpointError::new(
                "notification_stale",
                "notification is claimed by another owner",
            ));
        }
        let mut updated = row;
        updated.outbox_state = NotificationOutboxState::Claimed;
        updated.claim_token = Some(claim_token.to_string());
        updated.lease_expires_at_ms = Some(lease_expires_at_ms);
        updated.attempt = updated.attempt.checked_add(1).ok_or_else(|| {
            CheckpointError::new("notification_conflict", "notification attempt overflow")
        })?;
        updated.validate()?;
        ledger
            .notifications
            .insert(notification_id.to_string(), updated.clone());
        Ok(Some(updated))
    }

    fn get_host_interaction_notification(
        &self,
        notification_id: &str,
    ) -> CheckpointResult<Option<HostInteractionNotificationRecord>> {
        let ledger = self.controller_lock()?;
        Ok(ledger.notifications.get(notification_id).cloned())
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
        if !matches!(outcome, "delivered" | "ambiguous") {
            return Err(CheckpointError::new(
                "notification_conflict",
                "notification completion outcome must be delivered or ambiguous",
            ));
        }
        let mut ledger = self.controller_lock()?;
        let Some(row) = ledger.notifications.get(notification_id).cloned() else {
            return Ok(None);
        };
        if row.payload_digest != payload_digest {
            return Err(CheckpointError::new(
                "notification_conflict",
                "notification payload digest conflicts",
            ));
        }
        if row.outbox_state != NotificationOutboxState::Claimed
            || row.claim_token.as_deref() != Some(claim_token)
            || row.attempt != attempt
        {
            return Err(CheckpointError::new(
                "notification_stale",
                "notification owner or attempt is stale",
            ));
        }
        let mut updated = row;
        updated.outbox_state = if outcome == "delivered" {
            NotificationOutboxState::Delivered
        } else {
            NotificationOutboxState::Ambiguous
        };
        updated.claim_token = None;
        updated.lease_expires_at_ms = None;
        updated.delivered_at_ms = (outcome == "delivered").then_some(now_ms);
        updated.last_error = error.map(str::to_string);
        updated.validate()?;
        ledger
            .notifications
            .insert(notification_id.to_string(), updated.clone());
        Ok(Some(updated))
    }

    fn reconcile_host_interaction_notification(
        &self,
        notification_id: &str,
        payload_digest: &str,
        outcome: &str,
        now_ms: u64,
        abort_reason: Option<&str>,
    ) -> CheckpointResult<Option<HostInteractionNotificationRecord>> {
        if !matches!(outcome, "delivered" | "retry" | "abort") {
            return Err(CheckpointError::new(
                "notification_conflict",
                "notification reconciliation outcome is invalid",
            ));
        }
        if outcome == "abort"
            && abort_reason.is_none_or(|reason| {
                reason.trim().is_empty()
                    || reason.len() > crate::checkpoint::HOST_INTERACTION_CONTENT_MAX_UTF8_BYTES
            })
        {
            return Err(CheckpointError::new(
                "notification_conflict",
                "notification abort requires an explicit reason",
            ));
        }
        let mut ledger = self.controller_lock()?;
        let Some(row) = ledger.notifications.get(notification_id).cloned() else {
            return Ok(None);
        };
        if row.payload_digest != payload_digest {
            return Err(CheckpointError::new(
                "notification_conflict",
                "notification payload digest conflicts",
            ));
        }
        let target = match outcome {
            "delivered" => NotificationOutboxState::Delivered,
            "retry" => NotificationOutboxState::Pending,
            "abort" => NotificationOutboxState::Aborted,
            _ => unreachable!(),
        };
        if row.outbox_state == target {
            return Ok(Some(row));
        }
        if row.outbox_state != NotificationOutboxState::Ambiguous {
            return Err(CheckpointError::new(
                "notification_stale",
                "notification is not ambiguous",
            ));
        }
        let mut updated = row;
        updated.outbox_state = target;
        updated.delivered_at_ms = (target == NotificationOutboxState::Delivered).then_some(now_ms);
        updated.aborted_at_ms = (target == NotificationOutboxState::Aborted).then_some(now_ms);
        updated.abort_reason = (target == NotificationOutboxState::Aborted)
            .then(|| abort_reason.expect("abort reason validated").to_string());
        updated.validate()?;
        ledger
            .notifications
            .insert(notification_id.to_string(), updated.clone());
        Ok(Some(updated))
    }

    fn claim_controller_command_wake(
        &self,
        command_id: &str,
        command_digest: &str,
        claim_token: &str,
        lease_expires_at_ms: u64,
        now_ms: u64,
    ) -> CheckpointResult<Option<ControllerCommandReceipt>> {
        if claim_token.trim().is_empty() || lease_expires_at_ms <= now_ms {
            return Err(CheckpointError::new(
                "controller_command_outbox_invalid",
                "wake claim token must be non-empty and lease must be in the future",
            ));
        }
        let mut ledger = self.controller_lock()?;
        let Some((receipt, resolution)) = ledger.command_receipts.get(command_id).cloned() else {
            return Ok(None);
        };
        if receipt.command_digest != command_digest {
            return Err(CheckpointError::new(
                "controller_command_conflict",
                "wake command digest conflicts",
            ));
        }
        if receipt.outbox_action == "none"
            || receipt.outbox_state == "delivered"
            || receipt.outbox_state == "ambiguous"
        {
            if receipt.outbox_state == "ambiguous" {
                return Err(CheckpointError::new(
                    "controller_command_outbox_stale",
                    "ambiguous wake requires reconciliation before claiming",
                ));
            }
            return Ok(Some(receipt));
        }
        if let Some(lease) = ledger.wake_leases.get(command_id) {
            if receipt.outbox_state == "claimed"
                && lease.claim_token != claim_token
                && lease.lease_expires_at_ms > now_ms
            {
                return Err(CheckpointError::new(
                    "controller_command_outbox_stale",
                    "wake is claimed by another owner",
                ));
            }
        }
        let mut updated = receipt;
        updated.outbox_state = "claimed".to_string();
        updated.outbox_attempt = updated.outbox_attempt.checked_add(1).ok_or_else(|| {
            CheckpointError::new("controller_command_outbox_invalid", "wake attempt overflow")
        })?;
        updated.validate()?;
        ledger.wake_leases.insert(
            command_id.to_string(),
            WakeLease {
                claim_token: claim_token.to_string(),
                lease_expires_at_ms,
            },
        );
        ledger.command_receipts.insert(
            command_id.to_string(),
            (
                updated.clone(),
                resolution_with_receipt(&resolution, updated.clone()),
            ),
        );
        Ok(Some(updated))
    }

    fn complete_controller_command_wake(
        &self,
        command_id: &str,
        command_digest: &str,
        claim_token: &str,
        attempt: u64,
        outcome: &str,
        _now_ms: u64,
        _error: Option<&str>,
    ) -> CheckpointResult<Option<ControllerCommandReceipt>> {
        if !matches!(outcome, "delivered" | "ambiguous") {
            return Err(CheckpointError::new(
                "controller_command_outbox_invalid",
                "wake completion outcome must be delivered or ambiguous",
            ));
        }
        let mut ledger = self.controller_lock()?;
        let Some((receipt, resolution)) = ledger.command_receipts.get(command_id).cloned() else {
            return Ok(None);
        };
        if receipt.command_digest != command_digest {
            return Err(CheckpointError::new(
                "controller_command_conflict",
                "wake command digest conflicts",
            ));
        }
        let Some(lease) = ledger.wake_leases.get(command_id) else {
            return Err(CheckpointError::new(
                "controller_command_outbox_stale",
                "wake claim is missing",
            ));
        };
        if receipt.outbox_state != "claimed"
            || receipt.outbox_attempt != attempt
            || lease.claim_token != claim_token
        {
            return Err(CheckpointError::new(
                "controller_command_outbox_stale",
                "wake owner or attempt is stale",
            ));
        }
        let mut updated = receipt;
        updated.outbox_state = outcome.to_string();
        updated.validate()?;
        ledger.wake_leases.remove(command_id);
        ledger.command_receipts.insert(
            command_id.to_string(),
            (
                updated.clone(),
                resolution_with_receipt(&resolution, updated.clone()),
            ),
        );
        Ok(Some(updated))
    }

    fn reconcile_controller_command_wake(
        &self,
        command_id: &str,
        command_digest: &str,
        outcome: &str,
        _now_ms: u64,
    ) -> CheckpointResult<Option<ControllerCommandReceipt>> {
        if !matches!(outcome, "delivered" | "retry") {
            return Err(CheckpointError::new(
                "controller_command_outbox_invalid",
                "wake reconciliation outcome must be delivered or retry",
            ));
        }
        let mut ledger = self.controller_lock()?;
        let Some((receipt, resolution)) = ledger.command_receipts.get(command_id).cloned() else {
            return Ok(None);
        };
        if receipt.command_digest != command_digest {
            return Err(CheckpointError::new(
                "controller_command_conflict",
                "wake command digest conflicts",
            ));
        }
        if receipt.outbox_state != "ambiguous" {
            return Err(CheckpointError::new(
                "controller_command_outbox_stale",
                "wake is not ambiguous",
            ));
        }
        let mut updated = receipt;
        updated.outbox_state = outcome.to_string();
        updated.validate()?;
        ledger.command_receipts.insert(
            command_id.to_string(),
            (
                updated.clone(),
                resolution_with_receipt(&resolution, updated.clone()),
            ),
        );
        Ok(Some(updated))
    }

    fn reap_controller_command_wake(
        &self,
        command_id: &str,
        command_digest: &str,
        now_ms: u64,
    ) -> CheckpointResult<bool> {
        let mut ledger = self.controller_lock()?;
        let Some((receipt, resolution)) = ledger.command_receipts.get(command_id).cloned() else {
            return Ok(false);
        };
        if receipt.command_digest != command_digest {
            return Err(CheckpointError::new(
                "controller_command_conflict",
                "wake command digest conflicts",
            ));
        }
        let Some(lease) = ledger.wake_leases.get(command_id) else {
            return Ok(false);
        };
        if receipt.outbox_state != "claimed" || lease.lease_expires_at_ms > now_ms {
            return Ok(false);
        }
        let mut updated = receipt;
        updated.outbox_state = "pending".to_string();
        updated.validate()?;
        ledger.wake_leases.remove(command_id);
        ledger.command_receipts.insert(
            command_id.to_string(),
            (
                updated.clone(),
                resolution_with_receipt(&resolution, updated),
            ),
        );
        Ok(true)
    }

    fn delete_checkpoint(&self, checkpoint_key: &str) -> CheckpointResult<()> {
        // Keep both indexes under one lock order.  Resolving a receipt and
        // deleting its checkpoint cannot otherwise be linearized: a receipt
        // could be inserted after the checkpoint lock is released and survive
        // cleanup as an orphan.
        let mut checkpoints = self.lock()?;
        let mut receipts = self.receipt_lock()?;
        let mut controller = self.controller_lock()?;
        checkpoints.remove(checkpoint_key);
        receipts.retain(|_, receipt| receipt.handle.checkpoint_key != checkpoint_key);
        controller
            .host_interactions
            .retain(|_, record| record.checkpoint_key != checkpoint_key);
        controller
            .notifications
            .retain(|_, notification| notification.checkpoint_key != checkpoint_key);
        controller
            .command_receipts
            .retain(|_, (receipt, _)| receipt.handle.checkpoint_key != checkpoint_key);
        controller
            .commands
            .retain(|_, command| command.handle.checkpoint_key != checkpoint_key);
        let valid_wake_ids = controller
            .command_receipts
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        controller
            .wake_leases
            .retain(|command_id, _| valid_wake_ids.contains(command_id));
        Ok(())
    }
};
}
