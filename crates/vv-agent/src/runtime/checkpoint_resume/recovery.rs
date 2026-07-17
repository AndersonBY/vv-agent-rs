//! Claim recovery, reconciliation, and durable event delivery.

use super::*;

impl CheckpointResumeController {
    pub(super) fn create_new_checkpoint(&mut self, key: String) -> CheckpointResult<()> {
        let mut checkpoint = CheckpointV2 {
            run_definition: self.run_definition.clone(),
            checkpoint_key: key.clone(),
            task_id: self.task_id.clone(),
            root_run_id: self.run_id.clone(),
            trace_id: self.trace_id.clone(),
            run_definition_digest: self.run_definition_digest.clone(),
            messages: self.initial_messages.clone(),
            shared_state: self.initial_shared_state.clone(),
            budget_usage: self.initial_budget_usage.clone(),
            ..CheckpointV2::default()
        };
        self.snapshot_extensions_into(&mut checkpoint)?;
        let created_event = self.checkpoint_event(
            0,
            RunEventPayload::CheckpointCreated {
                checkpoint_key: key.clone(),
                resume_attempt: 1,
            },
            stable_event_id_for(&key, "checkpoint_created", &["0"]),
        )?;
        queue_event(&mut checkpoint, created_event)?;
        checkpoint.validate()?;
        if !self.store.create_checkpoint_v2(checkpoint.clone())? {
            return Err(CheckpointError::new(
                "checkpoint_key_conflict",
                format!("checkpoint key {key:?} was created concurrently"),
            ));
        }
        self.checkpoint = Some(checkpoint);
        self.created = true;
        self.deliver_pending_outbox()?;
        Ok(())
    }

    pub(super) fn ensure_claim(
        &mut self,
        cycle_index: u64,
    ) -> CheckpointResult<Option<AgentResult>> {
        if let Some(claim_token) = self.require_checkpoint()?.claim_token.as_deref() {
            if self.owned_claim_token.as_deref() == Some(claim_token)
                && self.require_checkpoint()?.claimed_cycle == Some(cycle_index)
            {
                return Ok(None);
            }
            if !self.first_claim_is_recovery {
                return Err(CheckpointError::new(
                    "checkpoint_claim_active",
                    "checkpoint claim is not owned by this execution",
                ));
            }
        }
        let now = now_ms()?;
        let expiry = now.checked_add(self.lease_duration_ms).ok_or_else(|| {
            CheckpointError::new("checkpoint_claim_invalid", "checkpoint lease overflow")
        })?;
        let mode = if self.first_claim_is_recovery {
            ClaimMode::Recovery
        } else {
            ClaimMode::Continue
        };
        let claim_token = format!("claim_{}", uuid::Uuid::new_v4().simple());
        let claimed = self.store.claim_checkpoint_v2(
            self.checkpoint_key()?,
            cycle_index,
            &claim_token,
            expiry,
            now,
            mode,
        )?;
        let claimed = claimed.ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_claim_active",
                "checkpoint cycle could not be claimed",
            )
        })?;
        self.checkpoint = Some(claimed);
        self.owned_claim_token = Some(claim_token);
        self.start_heartbeat()?;
        if mode == ClaimMode::Recovery {
            let checkpoint = self.require_checkpoint()?;
            let event = self.checkpoint_event(
                u32::try_from(checkpoint.cycle_index).unwrap_or(u32::MAX),
                RunEventPayload::CheckpointResumed {
                    checkpoint_key: checkpoint.checkpoint_key.clone(),
                    resume_attempt: checkpoint.resume_attempt,
                },
                self.stable_event_id(
                    "checkpoint_resumed",
                    &[&checkpoint.resume_attempt.to_string()],
                )?,
            )?;
            self.emit_durable(event)?;
            if let Some(interruption) = self.recover_ambiguous_operations()? {
                return Ok(Some(interruption));
            }
        }
        self.first_claim_is_recovery = false;
        Ok(None)
    }

    pub(super) fn recover_ambiguous_operations(&mut self) -> CheckpointResult<Option<AgentResult>> {
        let mut changed = false;
        {
            let checkpoint = self.require_checkpoint_mut()?;
            for entry in &mut checkpoint.model_call_journal {
                if entry.state == OperationState::Started {
                    entry.state = OperationState::Ambiguous;
                    entry.validate()?;
                    changed = true;
                }
            }
            for entry in &mut checkpoint.tool_journal {
                if entry.state == OperationState::Started {
                    entry.state = OperationState::Ambiguous;
                    entry.validate()?;
                    changed = true;
                }
            }
        }
        if changed {
            self.progress()?;
        }
        let ambiguous = self
            .require_checkpoint()?
            .model_call_journal
            .iter()
            .chain(self.require_checkpoint()?.tool_journal.iter())
            .filter(|entry| entry.state == OperationState::Ambiguous)
            .cloned()
            .collect::<Vec<_>>();
        for entry in ambiguous {
            let observation = observation(&entry);
            self.emit_ambiguous(&entry, &observation)?;
            let decision = self.reconciliation_decision(&entry, &observation)?;
            if decision.kind == ReconciliationDecisionKind::Defer {
                return Ok(Some(self.suspend_for_observation(
                    &entry,
                    observation,
                    true,
                )?));
            }
            if decision.kind == ReconciliationDecisionKind::Abort {
                let result = operator_abort_result(self.require_checkpoint()?, observation);
                return Ok(Some(result));
            }
            {
                let current = self.find_operation_mut(entry.kind, &entry.operation_id)?;
                apply_reconciliation_decision(current, &decision)?;
            }
            self.progress()?;
            let event = self.checkpoint_event(
                u32::try_from(entry.cycle_index).unwrap_or(u32::MAX),
                RunEventPayload::ReconciliationResolved {
                    checkpoint_key: self.checkpoint_key()?.to_string(),
                    operation_id: entry.operation_id.clone(),
                    operation_kind: entry.kind,
                    decision: decision.kind,
                },
                self.stable_event_id(
                    "reconciliation_resolved",
                    &[&entry.operation_id, &entry.attempt.to_string()],
                )?,
            )?;
            self.emit_durable(event)?;
        }
        Ok(None)
    }

    pub(super) fn reconciliation_decision(
        &mut self,
        entry: &OperationJournalEntry,
        observation: &ResumeObservation,
    ) -> CheckpointResult<ReconciliationDecision> {
        if let Some(provider) = &self.reconciliation_provider {
            let decision = provider.reconcile(observation)?;
            decision.validate()?;
            return Ok(decision);
        }
        if entry.kind == OperationKind::Model
            && self.config.ambiguous_model_policy == AmbiguousModelPolicy::RetryWithDuplicateRisk
        {
            let event = self.checkpoint_event(
                u32::try_from(entry.cycle_index).unwrap_or(u32::MAX),
                RunEventPayload::ModelRetryDuplicateRisk {
                    checkpoint_key: self.checkpoint_key()?.to_string(),
                    operation_id: entry.operation_id.clone(),
                    operation_kind: OperationKind::Model,
                    risk: "duplicate_model_request_and_cost".to_string(),
                },
                self.stable_event_id(
                    "model_retry_duplicate_risk",
                    &[&entry.operation_id, &(entry.attempt + 1).to_string()],
                )?,
            )?;
            self.emit_durable(event)?;
            return Ok(ReconciliationDecision::retry());
        }
        if entry.kind == OperationKind::Tool
            && self.config.ambiguous_tool_policy == AmbiguousToolPolicy::RetryIdempotentOnly
            && entry.idempotency_support == Some(ToolIdempotency::Supported)
        {
            return Ok(ReconciliationDecision::retry());
        }
        Ok(ReconciliationDecision::defer())
    }

    pub(super) fn suspend_for(
        &mut self,
        entry: &OperationJournalEntry,
    ) -> CheckpointResult<AgentResult> {
        self.suspend_for_observation(entry, observation(entry), false)
    }

    pub(super) fn suspend_for_observation(
        &mut self,
        entry: &OperationJournalEntry,
        observation: ResumeObservation,
        ambiguity_emitted: bool,
    ) -> CheckpointResult<AgentResult> {
        if !ambiguity_emitted {
            self.emit_ambiguous(entry, &observation)?;
        }
        let event = self.checkpoint_event(
            u32::try_from(entry.cycle_index).unwrap_or(u32::MAX),
            RunEventPayload::ReconciliationRequired {
                checkpoint_key: self.checkpoint_key()?.to_string(),
                operation_id: entry.operation_id.clone(),
                operation_kind: entry.kind,
                interruption_reason: "resume_requires_reconciliation".to_string(),
                resume_observation: observation.clone(),
            },
            self.stable_event_id(
                "reconciliation_required",
                &[&entry.operation_id, &entry.attempt.to_string()],
            )?,
        )?;
        self.emit_durable(event)?;
        self.require_checkpoint_mut()?.status = CheckpointStatus::ReconciliationRequired;
        let checkpoint = self.require_checkpoint()?.clone();
        let claim_token = checkpoint.claim_token.clone().ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_claim_active",
                "checkpoint suspension requires an active claim",
            )
        })?;
        if !self.store.suspend_checkpoint_v2(
            checkpoint.clone(),
            &claim_token,
            checkpoint.revision,
        )? {
            return Err(CheckpointError::new(
                "checkpoint_store_conflict",
                "failed to suspend checkpoint for reconciliation",
            ));
        }
        self.reload()?;
        self.owned_claim_token = None;
        self.stop_heartbeat();
        Ok(reconciliation_result(
            self.require_checkpoint()?,
            observation,
        ))
    }

    pub(super) fn emit_ambiguous(
        &mut self,
        entry: &OperationJournalEntry,
        observation: &ResumeObservation,
    ) -> CheckpointResult<()> {
        let event = self.checkpoint_event(
            u32::try_from(entry.cycle_index).unwrap_or(u32::MAX),
            RunEventPayload::OperationAmbiguous {
                checkpoint_key: self.checkpoint_key()?.to_string(),
                operation_id: entry.operation_id.clone(),
                operation_kind: entry.kind,
                risk: observation.risk.clone(),
                idempotency_support: observation.idempotency_support,
            },
            self.stable_event_id(
                "operation_ambiguous",
                &[&entry.operation_id, &entry.attempt.to_string()],
            )?,
        )?;
        self.emit_durable(event)
    }

    pub(super) fn emit_operation_replayed(
        &mut self,
        entry: &OperationJournalEntry,
    ) -> CheckpointResult<()> {
        let event = self.checkpoint_event(
            u32::try_from(entry.cycle_index).unwrap_or(u32::MAX),
            RunEventPayload::OperationReplayed {
                checkpoint_key: self.checkpoint_key()?.to_string(),
                operation_id: entry.operation_id.clone(),
                operation_kind: entry.kind,
                receipt_state: entry.state,
            },
            self.stable_event_id(
                "operation_replayed",
                &[&entry.operation_id, &entry.attempt.to_string()],
            )?,
        )?;
        self.emit_durable(event)
    }

    pub(super) fn emit_durable(&mut self, event: RunEvent) -> CheckpointResult<()> {
        self.queue_outbox_event(event)?;
        if self.require_checkpoint()?.claim_token.is_some() {
            self.progress()?;
        } else {
            return Err(CheckpointError::new(
                "checkpoint_claim_active",
                "checkpoint event enqueue requires an active claim",
            ));
        }
        self.deliver_pending_outbox()
    }

    pub(super) fn queue_outbox_event(&mut self, event: RunEvent) -> CheckpointResult<()> {
        queue_event(self.require_checkpoint_mut()?, event)
    }

    pub(super) fn deliver_pending_outbox(&mut self) -> CheckpointResult<()> {
        loop {
            let pending = self
                .require_checkpoint()?
                .event_outbox
                .iter()
                .find(|entry| entry.state == "pending")
                .cloned();
            let Some(pending) = pending else {
                return Ok(());
            };
            pending.verify_payload()?;
            let event: RunEvent =
                serde_json::from_value(pending.event.clone()).map_err(|error| {
                    CheckpointError::new(
                        "checkpoint_event_outbox_invalid",
                        format!("checkpoint event payload is invalid: {error}"),
                    )
                })?;
            let cursor = if let Some(store) = &self.event_store {
                match store
                    .append_once(&pending.event_id, &pending.payload_digest, &event)
                    .map_err(|error| {
                        CheckpointError::new("checkpoint_event_delivery_failed", error.to_string())
                    })? {
                    Some(cursor) => cursor,
                    None => {
                        store.append(&event).map_err(|error| {
                            CheckpointError::new(
                                "checkpoint_event_delivery_failed",
                                error.to_string(),
                            )
                        })?;
                        raw_event_cursor(&pending.event_id)?
                    }
                }
            } else {
                raw_event_cursor(&pending.event_id)?
            };
            (self.event_sink)(event)
                .map_err(|error| CheckpointError::new("checkpoint_event_delivery_failed", error))?;
            let checkpoint = self.require_checkpoint()?.clone();
            let recorded = self.store.record_event_delivery_v2(
                &checkpoint.checkpoint_key,
                checkpoint.claim_token.as_deref(),
                checkpoint.revision,
                &pending.event_id,
                &pending.payload_digest,
                cursor.clone(),
            )?;
            if !recorded {
                self.reload()?;
                let matching = self
                    .require_checkpoint()?
                    .event_outbox
                    .iter()
                    .find(|entry| entry.event_id == pending.event_id);
                if !matching.is_some_and(|entry| {
                    entry.state == "delivered"
                        && entry.payload_digest == pending.payload_digest
                        && entry.cursor.as_ref() == Some(&serde_json::to_value(&cursor).unwrap())
                }) {
                    return Err(CheckpointError::new(
                        "checkpoint_store_conflict",
                        "checkpoint event delivery lost its revision",
                    ));
                }
            } else {
                self.reload()?;
            }
        }
    }
}
