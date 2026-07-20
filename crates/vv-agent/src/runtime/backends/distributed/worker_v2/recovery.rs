//! Checkpoint validation, recovery, and commit transitions.

use super::*;

pub(super) fn load_v2(
    store: &dyn CheckpointStoreV2,
    checkpoint_key: &str,
) -> Result<CheckpointV2, String> {
    store
        .load_checkpoint_v2(checkpoint_key)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("No checkpoint found for key {checkpoint_key}"))
}

pub(super) fn validate_envelope_checkpoint_identity(
    envelope: &DistributedRunEnvelope,
    checkpoint: &CheckpointV2,
) -> Result<(), String> {
    checkpoint.validate().map_err(|error| error.to_string())?;
    let stored_digest = crate::checkpoint::run_definition_digest(&checkpoint.run_definition)
        .map_err(|error| error.to_string())?;
    if checkpoint.run_definition_digest != stored_digest {
        return Err("checkpoint_definition_mismatch".to_string());
    }
    let config = envelope
        .checkpoint_config
        .as_ref()
        .expect("validated v2 envelope has checkpoint_config");
    if checkpoint.checkpoint_key != config.key || checkpoint.task_id != envelope.task.task_id {
        return Err("checkpoint_identity_mismatch".to_string());
    }
    if checkpoint.root_run_id != envelope.root_run_id.as_deref().unwrap_or_default()
        || checkpoint.trace_id != envelope.trace_id.as_deref().unwrap_or_default()
    {
        return Err("checkpoint_run_identity_mismatch".to_string());
    }
    if checkpoint.run_definition_schema
        != envelope
            .run_definition_schema
            .as_deref()
            .unwrap_or_default()
        || checkpoint.run_definition_digest
            != envelope
                .run_definition_digest
                .as_deref()
                .unwrap_or_default()
    {
        return Err("checkpoint_definition_mismatch".to_string());
    }
    Ok(())
}

pub(super) fn validate_resume_attempt_observation(
    envelope: &DistributedRunEnvelope,
    checkpoint: &CheckpointV2,
    delivery: DistributedDeliveryMetadata,
) -> Result<(), String> {
    let observed = envelope.resume_attempt.unwrap_or_default();
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
    checkpoint: &CheckpointV2,
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
    checkpoint: &CheckpointV2,
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
        envelope
            .claim_mode
            .expect("validated v2 envelope has claim_mode")
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
    checkpoint: &mut CheckpointV2,
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

        let mut snapshot = progress.checkpoint.clone();
        let entry = match kind {
            OperationKind::Model => &mut snapshot.model_call_journal[index],
            OperationKind::Tool => &mut snapshot.tool_journal[index],
        };
        match decision.kind {
            ReconciliationDecisionKind::Retry => {
                entry.retry().map_err(|error| error.to_string())?;
            }
            ReconciliationDecisionKind::ReplaySuccess => {
                match kind {
                    OperationKind::Model => entry.response = decision.response,
                    OperationKind::Tool => entry.result = decision.result,
                }
                entry
                    .transition_to(OperationState::Succeeded)
                    .map_err(|error| error.to_string())?;
            }
            ReconciliationDecisionKind::RecordFailure => {
                let error = decision
                    .error
                    .expect("validated record_failure carries an error");
                entry.error = Some(OperationError::new(
                    error.code,
                    error.message,
                    error.retryable,
                ));
                entry
                    .transition_to(OperationState::Failed)
                    .map_err(|error| error.to_string())?;
            }
            ReconciliationDecisionKind::Abort => {
                let error = decision.error.expect("validated abort carries an error");
                let result = AgentResult::failed(format!("{}: {}", error.code, error.message));
                let cycle_index = entry.cycle_index;
                snapshot.status = CheckpointStatus::Failed;
                snapshot.terminal_result = Some(result.to_dict());
                snapshot.cycle_index = cycle_index;
                return Ok(RecoveryDisposition::Abort(Box::new(snapshot)));
            }
            ReconciliationDecisionKind::Defer => {
                unreachable!("defer returned before mutating the journal")
            }
        }
        progress.persist(snapshot)?;
    }

    if progress.checkpoint.has_ambiguous_operation() {
        Ok(RecoveryDisposition::Suspend)
    } else {
        Ok(RecoveryDisposition::Continue)
    }
}

pub(super) fn resume_observation(
    entry: &crate::runtime::state_v2::OperationJournalEntry,
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
    entry: &crate::runtime::state_v2::OperationJournalEntry,
) -> ReconciliationDecision {
    match entry.kind {
        OperationKind::Model
            if config.ambiguous_model_policy
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
        .suspend_checkpoint_v2(snapshot, &progress.claim_token, expected_revision)
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
    mut checkpoint: CheckpointV2,
    progress: &mut DistributedCheckpointProgress,
    heartbeat_status: &LeaseHeartbeatStatus,
    cycle_index: u64,
) -> Result<(), String> {
    align_active_claim(&mut checkpoint, &progress.checkpoint);
    checkpoint.cycle_index = cycle_index;
    checkpoint.status = CheckpointStatus::Running;
    checkpoint.terminal_result = None;
    checkpoint.terminal_acknowledged = false;
    let expected_revision = progress.checkpoint.revision;
    heartbeat_status.begin_commit()?;
    if !progress
        .store
        .commit_checkpoint_v2(checkpoint, &progress.claim_token, expected_revision)
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
    terminal: CheckpointV2,
    progress: &mut DistributedCheckpointProgress,
    _cycle_index: u64,
) -> Result<(AgentResult, u64), String> {
    let terminal_status = terminal.status;
    let terminal_result = terminal
        .terminal_result
        .clone()
        .ok_or_else(|| "distributed v2 terminal outcome requires terminal_result".to_string())?;
    if !terminal_status.is_terminal() {
        return Err("distributed v2 terminal outcome requires a terminal status".to_string());
    }
    let result = AgentResult::from_dict(&terminal_result)?;

    let mut pending = progress.checkpoint.clone();
    pending.extension_state = terminal.extension_state;
    let persisted = progress.persist(pending)?;
    Ok((result, persisted.revision))
}

pub(super) fn align_active_claim(snapshot: &mut CheckpointV2, current: &CheckpointV2) {
    snapshot.revision = current.revision;
    snapshot.resume_attempt = current.resume_attempt;
    snapshot.claim_token = current.claim_token.clone();
    snapshot.claimed_cycle = current.claimed_cycle;
    snapshot.lease_expires_at_ms = current.lease_expires_at_ms;
    snapshot.terminal_acknowledged = current.terminal_acknowledged;
}

pub(super) fn terminal_replay(checkpoint: &CheckpointV2) -> Result<CycleDispatchResult, String> {
    let result = AgentResult::from_dict(
        checkpoint
            .terminal_result
            .as_ref()
            .ok_or_else(|| "terminal checkpoint is missing terminal_result".to_string())?,
    )?;
    Ok(CycleDispatchResult::terminal_replay(
        result,
        checkpoint.revision,
    ))
}

pub(super) fn reconciliation_candidate(checkpoint: &CheckpointV2) -> Result<AgentResult, String> {
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
        resume_observation: Some(observation),
        final_answer: None,
        wait_reason: None,
        error: None,
        error_code: None,
        shared_state: checkpoint.shared_state.clone(),
        token_usage: crate::runtime::summarize_task_token_usage(&checkpoint.cycles),
    })
}
