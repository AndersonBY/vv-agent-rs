//! Checkpoint validation, recovery, and commit transitions.

use super::*;

use crate::checkpoint::{
    ControllerCommandVariant, HostInteractionRecoveryEnvelope, HostInteractionRequest,
    HOST_INTERACTION_RECOVERY_SCHEMA, HOST_INTERACTION_REQUEST_SCHEMA,
};
use crate::events::RunEventPayload;

pub(super) fn consume_controller_wakes(
    store: &dyn CheckpointStore,
    checkpoint: crate::runtime::Checkpoint,
    lease_duration_ms: u64,
) -> Result<(crate::runtime::Checkpoint, bool), String> {
    if checkpoint.status != CheckpointStatus::Running || checkpoint.claim_token.is_some() {
        return Ok((checkpoint, false));
    }

    let now_ms = now_unix_ms()?;
    let wakes = store
        .reap_controller_command_wakes(&checkpoint.checkpoint_key, now_ms)
        .map_err(|error| error.to_string())?;
    let mut current = checkpoint;
    let mut consumed_host_response = false;
    for wake in wakes {
        if wake.outbox_state != "pending" {
            return Err("controller wake reaper returned a non-pending wake".to_string());
        }
        if wake.checkpoint_key != current.checkpoint_key
            || wake.handle.checkpoint_key != current.checkpoint_key
            || wake.outbox_action != "recovery_dispatch"
        {
            return Err("controller wake escaped its checkpoint scope".to_string());
        }
        let command = store
            .get_controller_command(&wake.command_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "controller wake command payload is missing".to_string())?;
        if command.handle.checkpoint_key != current.checkpoint_key
            || command.command_id != wake.command_id
            || command.command_digest != wake.command_digest
        {
            return Err("controller wake command identity is inconsistent".to_string());
        }

        let host_recovery = match &command.command {
            ControllerCommandVariant::HostInteractionResponse {
                interaction_id,
                request_digest,
                ..
            } => Some((
                host_request_from_event(&current, interaction_id, request_digest)?,
                command.command_id.clone(),
            )),
            ControllerCommandVariant::Resume => {
                match store
                    .find_resolved_pending_host_interaction(&current.checkpoint_key)
                    .map_err(|error| error.to_string())?
                {
                    Some(record) => {
                        if record.checkpoint_key != current.checkpoint_key
                            || record.state != "resolved_pending"
                        {
                            return Err("resume wake resolved host record is stale".to_string());
                        }
                        let response_command_id = record.command_id.clone().ok_or_else(|| {
                            "resume wake resolved host record has no response command".to_string()
                        })?;
                        let response_command = store
                            .get_controller_command(&response_command_id)
                            .map_err(|error| error.to_string())?
                            .ok_or_else(|| "resume wake response command is missing".to_string())?;
                        if !matches!(
                            response_command.command,
                            ControllerCommandVariant::HostInteractionResponse { .. }
                        ) || response_command.handle.checkpoint_key != current.checkpoint_key
                        {
                            return Err("resume wake response command is stale".to_string());
                        }
                        Some((record.request, response_command_id))
                    }
                    None => {
                        if checkpoint_has_unconsumed_host_interaction(&current)? {
                            return Err(
                                "host interaction recovery record is missing for a resume wake"
                                    .to_string(),
                            );
                        }
                        None
                    }
                }
            }
            _ => {
                return Err("non-waking controller command has a recovery wake".to_string());
            }
        };

        let claim_token = format!("distributed-host-response:{}", wake.command_id);
        let lease_expires_at_ms = now_ms
            .checked_add(lease_duration_ms.max(1_000))
            .ok_or_else(|| "controller wake claim lease overflow".to_string())?;

        if let Some((request, response_command_id)) = host_recovery {
            let record_id = crate::checkpoint::record_id_for(&current.checkpoint_key, &request);
            store
                .reap_host_interaction_record(&record_id, &current.checkpoint_key, now_ms)
                .map_err(|error| error.to_string())?;
            current = load_checkpoint(store, &current.checkpoint_key)?;
            let Some(claimed_wake) = store
                .claim_controller_command_wake(
                    &wake.command_id,
                    &wake.command_digest,
                    &claim_token,
                    lease_expires_at_ms,
                    now_ms,
                )
                .map_err(|error| error.to_string())?
            else {
                return Err("controller wake claim was lost before recovery".to_string());
            };
            if claimed_wake.outbox_state != "claimed" || claimed_wake.outbox_attempt == 0 {
                return Err("controller wake claim did not retain ownership".to_string());
            }
            let recovery = HostInteractionRecoveryEnvelope {
                schema_version: HOST_INTERACTION_RECOVERY_SCHEMA.to_string(),
                record_id: record_id.clone(),
                checkpoint_key: current.checkpoint_key.clone(),
                run_id: current.root_run_id.clone(),
                trace_id: current.trace_id.clone(),
                claim_mode: "recovery".to_string(),
                resume_attempt: current.resume_attempt,
                expected_revision: current.revision,
                logical_cycle: request.logical_cycle,
                interaction_id: request.interaction_id.clone(),
                operation_id: request.operation_id.clone(),
                tool_call_id: request.tool_call_id.clone(),
                request_digest: request.request_digest.clone(),
                command_id: response_command_id,
            };
            let result = store
                .claim_and_consume_host_interaction_response(recovery)
                .map_err(|error| error.to_string())?;
            result.validate().map_err(|error| error.to_string())?;
            if !matches!(result.kind.as_str(), "applied" | "replayed")
                || result.record_id != record_id
            {
                return Err("host interaction recovery did not consume the response".to_string());
            }
            let completed = store
                .complete_controller_command_wake(
                    &wake.command_id,
                    &wake.command_digest,
                    &claim_token,
                    claimed_wake.outbox_attempt,
                    "delivered",
                    now_unix_ms()?,
                    None,
                )
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "controller wake disappeared during completion".to_string())?;
            if completed.outbox_state != "delivered" {
                return Err("controller wake completion was not durable".to_string());
            }
            current = load_checkpoint(store, &current.checkpoint_key)?;
            consumed_host_response = result.kind == "applied";
            if consumed_host_response && Some(current.revision) != result.checkpoint_revision {
                return Err("host response execution claim changed".to_string());
            }
            break;
        } else {
            let Some(claimed_wake) = store
                .claim_controller_command_wake(
                    &wake.command_id,
                    &wake.command_digest,
                    &claim_token,
                    lease_expires_at_ms,
                    now_ms,
                )
                .map_err(|error| error.to_string())?
            else {
                return Err("controller wake claim was lost before completion".to_string());
            };
            if claimed_wake.outbox_state != "claimed" || claimed_wake.outbox_attempt == 0 {
                return Err("controller wake claim did not retain ownership".to_string());
            }
            let completed = store
                .complete_controller_command_wake(
                    &wake.command_id,
                    &wake.command_digest,
                    &claim_token,
                    claimed_wake.outbox_attempt,
                    "delivered",
                    now_unix_ms()?,
                    None,
                )
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "controller wake disappeared during completion".to_string())?;
            if completed.outbox_state != "delivered" {
                return Err("controller wake completion was not durable".to_string());
            }
            current = load_checkpoint(store, &current.checkpoint_key)?;
        }
    }
    Ok((current, consumed_host_response))
}

fn host_request_from_event(
    checkpoint: &crate::runtime::Checkpoint,
    interaction_id: &str,
    request_digest: &str,
) -> Result<HostInteractionRequest, String> {
    for entry in &checkpoint.event_outbox {
        let event = serde_json::from_value::<RunEvent>(entry.event.clone())
            .map_err(|error| format!("host interaction recovery event is invalid: {error}"))?;
        let RunEventPayload::HostInteractionRequested {
            checkpoint_key,
            interaction_id: event_interaction_id,
            logical_cycle,
            operation_id,
            tool_call_id,
            request_digest: event_request_digest,
            prompt,
            ..
        } = event.payload
        else {
            continue;
        };
        if checkpoint_key == checkpoint.checkpoint_key
            && event_interaction_id == interaction_id
            && event_request_digest == request_digest
        {
            let request = HostInteractionRequest {
                schema_version: HOST_INTERACTION_REQUEST_SCHEMA.to_string(),
                interaction_id: event_interaction_id,
                logical_cycle,
                operation_id,
                tool_call_id,
                request_digest: event_request_digest,
                prompt,
            };
            request.validate().map_err(|error| error.to_string())?;
            return Ok(request);
        }
    }
    Err("host interaction recovery request is missing".to_string())
}

fn checkpoint_has_unconsumed_host_interaction(
    checkpoint: &crate::runtime::Checkpoint,
) -> Result<bool, String> {
    let mut requested = std::collections::BTreeSet::new();
    let mut consumed = std::collections::BTreeSet::new();
    for entry in &checkpoint.event_outbox {
        let event = serde_json::from_value::<RunEvent>(entry.event.clone())
            .map_err(|error| format!("host interaction recovery event is invalid: {error}"))?;
        match event.payload {
            RunEventPayload::HostInteractionRequested {
                interaction_id,
                request_digest,
                ..
            } => {
                requested.insert((interaction_id, request_digest));
            }
            RunEventPayload::HostInteractionResponseConsumed {
                interaction_id,
                request_digest,
                ..
            } => {
                consumed.insert((interaction_id, request_digest));
            }
            _ => {}
        }
    }
    Ok(requested.into_iter().any(|key| !consumed.contains(&key)))
}

pub(super) fn load_checkpoint(
    store: &dyn CheckpointStore,
    checkpoint_key: &str,
) -> Result<Checkpoint, String> {
    store
        .load_checkpoint(checkpoint_key)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("No checkpoint found for key {checkpoint_key}"))
}

pub(super) fn validate_envelope_checkpoint_identity(
    envelope: &DistributedRunEnvelope,
    checkpoint: &Checkpoint,
) -> Result<(), String> {
    checkpoint.validate().map_err(|error| error.to_string())?;
    let stored_digest = crate::checkpoint::run_definition_digest(&checkpoint.run_definition)
        .map_err(|error| error.to_string())?;
    if checkpoint.run_definition_digest != stored_digest {
        return Err("checkpoint_definition_mismatch".to_string());
    }
    let config = &envelope.checkpoint_config;
    if checkpoint.checkpoint_key != config.key || checkpoint.task_id != envelope.task.task_id {
        return Err("checkpoint_identity_mismatch".to_string());
    }
    if checkpoint.root_run_id != envelope.root_run_id || checkpoint.trace_id != envelope.trace_id {
        return Err("checkpoint_run_identity_mismatch".to_string());
    }
    if checkpoint.run_definition_schema != envelope.run_definition_schema
        || checkpoint.run_definition_digest != envelope.run_definition_digest
    {
        return Err("checkpoint_definition_mismatch".to_string());
    }
    Ok(())
}

pub(super) fn validate_resume_attempt_observation(
    envelope: &DistributedRunEnvelope,
    checkpoint: &Checkpoint,
    delivery: DistributedDeliveryMetadata,
) -> Result<(), String> {
    let observed = envelope.resume_attempt;
    if checkpoint.resume_attempt == observed
        || delivery.is_redelivery() && checkpoint.resume_attempt > observed
    {
        Ok(())
    } else {
        Err("checkpoint_resume_attempt_mismatch".to_string())
    }
}

pub(super) fn validate_claimed_resume_attempt(
    resume_attempt_before_claim: u64,
    checkpoint: &Checkpoint,
    claim_mode: ClaimMode,
) -> Result<(), String> {
    let expected = resume_attempt_before_claim
        .checked_add(u64::from(claim_mode == ClaimMode::Recovery))
        .ok_or_else(|| "checkpoint_resume_attempt_invalid".to_string())?;
    if checkpoint.resume_attempt != expected {
        return Err("checkpoint_resume_attempt_mismatch".to_string());
    }
    Ok(())
}

pub(super) fn effective_claim_mode(
    envelope: &DistributedRunEnvelope,
    checkpoint: &Checkpoint,
    delivery: DistributedDeliveryMetadata,
    now_ms: u64,
) -> ClaimMode {
    if delivery.is_redelivery()
        || checkpoint.status == CheckpointStatus::ReconciliationRequired
        || checkpoint
            .lease_expires_at_ms
            .is_some_and(|expiry| expiry <= now_ms)
    {
        ClaimMode::Recovery
    } else {
        envelope.claim_mode
    }
}

pub(super) fn validate_extension_capabilities(
    config: &DistributedCheckpointConfig,
    capabilities: &ResolvedDistributedCapabilities,
) -> Result<(), String> {
    for namespace in &config.required_extension_namespaces {
        if !capabilities
            .checkpoint_extensions
            .iter()
            .any(|extension| extension.descriptor.namespace == *namespace)
        {
            return Err(format!(
                "required checkpoint extension {namespace} is unavailable"
            ));
        }
    }
    Ok(())
}

pub(super) fn initialize_extensions(
    config: &DistributedCheckpointConfig,
    capabilities: &ResolvedDistributedCapabilities,
    progress: &mut DistributedCheckpointProgress,
) -> Result<(), String> {
    let mut snapshot = progress.checkpoint.clone();
    let mut changed = false;
    for resolved in &capabilities.checkpoint_extensions {
        let namespace = resolved.descriptor.namespace.as_str();
        if let Some(entry) = snapshot.extension_state.get(namespace) {
            if entry.version != resolved.extension.version() {
                return Err(format!(
                    "checkpoint extension {namespace} version mismatch: expected {}, got {}",
                    resolved.extension.version(),
                    entry.version
                ));
            }
            resolved
                .extension
                .restore(&entry.state)
                .map_err(|error| error.to_string())?;
        } else {
            let state = resolved
                .extension
                .snapshot()
                .map_err(|error| error.to_string())?;
            snapshot.extension_state.insert(
                namespace.to_string(),
                ExtensionStateEntry {
                    version: resolved.extension.version().to_string(),
                    required: resolved.descriptor.required || resolved.extension.required(),
                    state,
                },
            );
            changed = true;
        }
    }
    validate_extension_state_size(&snapshot.extension_state, config.max_extension_state_bytes)
        .map_err(|error| error.to_string())?;
    if changed {
        progress.persist(snapshot)?;
    }
    Ok(())
}

pub(super) fn snapshot_extensions(
    config: &DistributedCheckpointConfig,
    capabilities: &ResolvedDistributedCapabilities,
    checkpoint: &mut Checkpoint,
) -> Result<(), String> {
    for resolved in &capabilities.checkpoint_extensions {
        checkpoint.extension_state.insert(
            resolved.descriptor.namespace.clone(),
            ExtensionStateEntry {
                version: resolved.extension.version().to_string(),
                required: resolved.descriptor.required || resolved.extension.required(),
                state: resolved
                    .extension
                    .snapshot()
                    .map_err(|error| error.to_string())?,
            },
        );
    }
    validate_extension_state_size(
        &checkpoint.extension_state,
        config.max_extension_state_bytes,
    )
    .map_err(|error| error.to_string())
}

pub(super) fn reconcile_recovery(
    config: &DistributedCheckpointConfig,
    capabilities: &ResolvedDistributedCapabilities,
    progress: &mut DistributedCheckpointProgress,
) -> Result<RecoveryDisposition, String> {
    let mut snapshot = progress.checkpoint.clone();
    let mut changed = false;
    for entry in snapshot
        .model_call_journal
        .iter_mut()
        .chain(snapshot.tool_journal.iter_mut())
    {
        if entry.state == OperationState::Started {
            entry.mark_ambiguous().map_err(|error| error.to_string())?;
            changed = true;
        }
    }
    if changed {
        progress.persist(snapshot)?;
    }

    let positions = progress
        .checkpoint
        .model_call_journal
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.state == OperationState::Ambiguous)
        .map(|(index, _)| (OperationKind::Model, index))
        .chain(
            progress
                .checkpoint
                .tool_journal
                .iter()
                .enumerate()
                .filter(|(_, entry)| entry.state == OperationState::Ambiguous)
                .map(|(index, _)| (OperationKind::Tool, index)),
        )
        .collect::<Vec<_>>();

    let mut deferred_decisions = Vec::new();
    for (kind, index) in positions {
        let entry = match kind {
            OperationKind::Model => &progress.checkpoint.model_call_journal[index],
            OperationKind::Tool => &progress.checkpoint.tool_journal[index],
        };
        let observation = resume_observation(entry)?;
        let decision = if let Some(provider) = &capabilities.reconciliation_provider {
            provider
                .reconcile(&observation)
                .map_err(|error| error.to_string())?
        } else {
            default_reconciliation_decision(config, entry)
        };
        decision.validate().map_err(|error| error.to_string())?;
        if decision.kind == ReconciliationDecisionKind::Defer {
            continue;
        }
        if decision.kind == ReconciliationDecisionKind::AcceptDeferred {
            if kind != OperationKind::Tool {
                return Err("accept_deferred is only valid for tool operations".to_string());
            }
            let handle = decision
                .handle
                .clone()
                .ok_or_else(|| "accept_deferred requires a handle".to_string())?;
            if handle.checkpoint_key != progress.checkpoint.checkpoint_key
                || handle.operation_id != entry.operation_id
                || handle.attempt != entry.attempt
                || handle.request_digest != entry.request_digest
            {
                return Err("accept_deferred handle identity mismatch".to_string());
            }
            deferred_decisions.push(crate::checkpoint::AcceptDeferredDecision::new(handle));
            continue;
        }

        let mut snapshot = progress.checkpoint.clone();
        let unknown_tool_outcome = kind == OperationKind::Tool
            && decision.kind == ReconciliationDecisionKind::RecordFailure
            && decision
                .error
                .as_ref()
                .is_some_and(|error| error.code == "tool_outcome_unknown");
        let checkpoint_key = snapshot.checkpoint_key.clone();
        let receipt_result = crate::runtime::checkpoint_resume::reconciliation_tool_result(
            match kind {
                OperationKind::Model => &snapshot.model_call_journal[index],
                OperationKind::Tool => &snapshot.tool_journal[index],
            },
            &decision,
        )
        .map_err(|error| error.to_string())?;
        let entry = match kind {
            OperationKind::Model => &mut snapshot.model_call_journal[index],
            OperationKind::Tool => &mut snapshot.tool_journal[index],
        };
        match decision.kind {
            ReconciliationDecisionKind::Retry => {
                entry.retry().map_err(|error| error.to_string())?;
            }
            ReconciliationDecisionKind::ReplaySuccess
            | ReconciliationDecisionKind::RecordFailure => {
                crate::runtime::checkpoint_resume::apply_reconciliation_decision(
                    entry,
                    &decision,
                    &checkpoint_key,
                    unknown_tool_outcome.then_some(&observation),
                )
                .map_err(|error| error.to_string())?;
            }
            ReconciliationDecisionKind::Abort => {
                decision
                    .error
                    .as_ref()
                    .expect("validated abort carries an error");
                let observation = resume_observation(entry)?;
                let mut result = AgentResult::failed_with_code(
                    "operator_abort_with_unknown_outcome",
                    "Operator accepted that the external outcome is unknown.",
                    false,
                );
                result.messages = snapshot.messages.clone();
                result.cycles = snapshot.cycles.clone();
                result.partial_output = crate::types::last_assistant_output(&snapshot.cycles);
                result.budget_usage = snapshot.budget_usage.clone();
                result.checkpoint_key = Some(snapshot.checkpoint_key.clone());
                result.resume_observations = vec![observation];
                result.shared_state = snapshot.shared_state.clone();
                result.token_usage =
                    crate::runtime::summarize_task_token_usage(&snapshot.model_calls);
                let cycle_index = entry.cycle_index;
                snapshot.status = CheckpointStatus::Failed;
                snapshot.terminal_result = Some(result.to_dict());
                snapshot.cycle_index = cycle_index;
                return Ok(RecoveryDisposition::Abort(Box::new(snapshot)));
            }
            ReconciliationDecisionKind::AcceptDeferred => {
                unreachable!("accept_deferred decisions are collected for one batch CAS")
            }
            ReconciliationDecisionKind::Defer => {
                unreachable!("defer returned before mutating the journal")
            }
        }
        if let Some(result) = receipt_result {
            let entry = snapshot.tool_journal[index].clone();
            let receipt = crate::runtime::state::receipt_event(&snapshot, &entry, &result)
                .map_err(|error| error.to_string())?;
            crate::runtime::state::append_event_outbox_once(&mut snapshot.event_outbox, receipt)
                .map_err(|error| error.to_string())?;
        }
        progress.persist(snapshot)?;
    }

    if !deferred_decisions.is_empty() {
        progress.accept_deferred_batch(&deferred_decisions)?;
        return Ok(RecoveryDisposition::Deferred);
    }

    if progress.checkpoint.has_ambiguous_operation() {
        Ok(RecoveryDisposition::Suspend)
    } else {
        Ok(RecoveryDisposition::Continue)
    }
}

pub(super) fn resume_observation(
    entry: &crate::runtime::state::OperationJournalEntry,
) -> Result<ResumeObservation, String> {
    let (risk, idempotency_support) = match entry.kind {
        OperationKind::Model => ("possible_duplicate_model_request_and_cost", None),
        OperationKind::Tool => (
            "possible_duplicate_tool_side_effect",
            Some(
                entry
                    .idempotency_support
                    .unwrap_or(ToolIdempotency::Unknown),
            ),
        ),
    };
    let observation = ResumeObservation {
        operation_id: entry.operation_id.clone(),
        operation_kind: entry.kind,
        cycle_index: entry.cycle_index,
        state: OperationState::Ambiguous,
        risk: risk.to_string(),
        idempotency_support,
    };
    observation.validate().map_err(|error| error.to_string())?;
    Ok(observation)
}

pub(super) fn default_reconciliation_decision(
    config: &DistributedCheckpointConfig,
    entry: &crate::runtime::state::OperationJournalEntry,
) -> ReconciliationDecision {
    match entry.kind {
        OperationKind::Model
            if entry.attempt < 2
                && config.ambiguous_model_policy
                    == crate::checkpoint::AmbiguousModelPolicy::RetryWithDuplicateRisk =>
        {
            ReconciliationDecision::retry()
        }
        OperationKind::Tool
            if config.ambiguous_tool_policy
                == crate::checkpoint::AmbiguousToolPolicy::RetryIdempotentOnly
                && entry.idempotency_support == Some(ToolIdempotency::Supported) =>
        {
            ReconciliationDecision::retry()
        }
        OperationKind::Tool
            if config.ambiguous_tool_policy
                == crate::checkpoint::AmbiguousToolPolicy::SurfaceToModel =>
        {
            ReconciliationDecision::record_failure(crate::checkpoint::ReconciliationError::new(
                "tool_outcome_unknown",
                "The tool outcome is unknown.",
                false,
            ))
        }
        _ => ReconciliationDecision::defer(),
    }
}

pub(super) fn suspend_reconciliation(
    progress: &mut DistributedCheckpointProgress,
    heartbeat_status: &LeaseHeartbeatStatus,
) -> Result<(), String> {
    let mut snapshot = progress.checkpoint.clone();
    align_active_claim(&mut snapshot, &progress.checkpoint);
    let expected_revision = progress.checkpoint.revision;
    heartbeat_status.begin_commit()?;
    if !progress
        .store
        .suspend_checkpoint(snapshot, &progress.claim_token, expected_revision)
        .map_err(|error| error.to_string())?
    {
        return Err(format!(
            "checkpoint changed while suspending reconciliation for {}",
            progress.checkpoint.checkpoint_key
        ));
    }
    heartbeat_status.mark_commit_succeeded()
}

pub(super) fn commit_cycle(
    mut checkpoint: Checkpoint,
    progress: &mut DistributedCheckpointProgress,
    heartbeat_status: &LeaseHeartbeatStatus,
    event_store: Option<&dyn RunEventStore>,
    event_sink: &CheckpointEventSink,
    cycle_index: u64,
) -> Result<(), String> {
    align_active_claim(&mut checkpoint, &progress.checkpoint);
    if checkpoint
        .event_outbox
        .iter()
        .any(|entry| entry.state == "pending")
    {
        checkpoint.cycle_index = progress.checkpoint.cycle_index;
        progress.persist(checkpoint)?;
        progress.deliver_pending_outbox(event_store, event_sink)?;
        checkpoint = progress.checkpoint.clone();
    }
    checkpoint.cycle_index = cycle_index;
    checkpoint.status = CheckpointStatus::Running;
    checkpoint.terminal_result = None;
    checkpoint.terminal_acknowledged = false;
    let expected_revision = progress.checkpoint.revision;
    heartbeat_status.begin_commit()?;
    if !progress
        .store
        .commit_checkpoint(checkpoint, &progress.claim_token, expected_revision)
        .map_err(|error| error.to_string())?
    {
        return Err(format!(
            "checkpoint changed while committing cycle {cycle_index} for {}",
            progress.checkpoint.checkpoint_key
        ));
    }
    heartbeat_status.mark_commit_succeeded()
}

pub(super) fn prepare_terminal_candidate(
    terminal: Checkpoint,
    progress: &mut DistributedCheckpointProgress,
    _cycle_index: u64,
) -> Result<(AgentResult, u64), String> {
    let terminal_status = terminal.status;
    let terminal_result = terminal
        .terminal_result
        .clone()
        .ok_or_else(|| "distributed terminal outcome requires terminal_result".to_string())?;
    if !terminal_status.is_terminal() {
        return Err("distributed terminal outcome requires a terminal status".to_string());
    }
    let mut pending = progress.checkpoint.clone();
    pending.extension_state = terminal.extension_state;
    let persisted = progress.persist(pending)?;
    let authoritative = persisted
        .terminal_result
        .as_ref()
        .unwrap_or(&terminal_result);
    Ok((AgentResult::from_dict(authoritative)?, persisted.revision))
}

pub(super) fn align_active_claim(snapshot: &mut Checkpoint, current: &Checkpoint) {
    snapshot.revision = current.revision;
    snapshot.resume_attempt = current.resume_attempt;
    snapshot.claim_token = current.claim_token.clone();
    snapshot.claimed_cycle = current.claimed_cycle;
    snapshot.lease_expires_at_ms = current.lease_expires_at_ms;
    snapshot.terminal_acknowledged = current.terminal_acknowledged;
}

pub(super) fn reconciliation_candidate(checkpoint: &Checkpoint) -> Result<AgentResult, String> {
    let entry = checkpoint
        .model_call_journal
        .iter()
        .chain(checkpoint.tool_journal.iter())
        .find(|entry| entry.state == OperationState::Ambiguous)
        .ok_or_else(|| "reconciliation checkpoint is missing an ambiguous operation".to_string())?;
    let observation = resume_observation(entry)?;
    Ok(AgentResult {
        status: crate::types::AgentStatus::ReconciliationRequired,
        messages: checkpoint.messages.clone(),
        cycles: checkpoint.cycles.clone(),
        completion_reason: None,
        completion_tool_name: None,
        partial_output: crate::types::last_assistant_output(&checkpoint.cycles),
        budget_usage: checkpoint.budget_usage.clone(),
        budget_exhaustion: None,
        checkpoint_key: Some(checkpoint.checkpoint_key.clone()),
        resume_observations: vec![observation],
        final_answer: None,
        wait_reason: None,
        error: None,
        error_code: None,
        shared_state: checkpoint.shared_state.clone(),
        token_usage: crate::runtime::summarize_task_token_usage(&checkpoint.model_calls),
    })
}
