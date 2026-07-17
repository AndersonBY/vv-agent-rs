use super::*;

pub fn claim_candidate(
    checkpoint: &CheckpointV2,
    cycle_index: u64,
    now_ms: u64,
    claim_mode: ClaimMode,
) -> CheckpointResult<bool> {
    if cycle_index == 0 || cycle_index > MAX_WIRE_INTEGER {
        return Err(CheckpointError::new(
            "checkpoint_claim_invalid",
            "claimed cycle must be positive and JSON-safe",
        ));
    }
    if now_ms > MAX_WIRE_INTEGER {
        return Err(CheckpointError::new(
            "checkpoint_claim_invalid",
            "now_ms is outside the JSON-safe range",
        ));
    }
    if checkpoint.terminal_result.is_some() || checkpoint.status.is_terminal() {
        return Ok(false);
    }
    if !matches!(
        checkpoint.status,
        CheckpointStatus::Running | CheckpointStatus::ReconciliationRequired
    ) {
        return Ok(false);
    }
    if checkpoint.cycle_index.checked_add(1) != Some(cycle_index) {
        return Ok(false);
    }
    if (checkpoint.status == CheckpointStatus::ReconciliationRequired
        || checkpoint.has_ambiguous_operation())
        && claim_mode != ClaimMode::Recovery
    {
        return Ok(false);
    }
    if let Some(expiry) = checkpoint.lease_expires_at_ms {
        if expiry > now_ms {
            return Ok(false);
        }
        if claim_mode != ClaimMode::Recovery {
            return Ok(false);
        }
    }
    Ok(true)
}

pub fn apply_claim(
    checkpoint: &mut CheckpointV2,
    cycle_index: u64,
    claim_token: &str,
    lease_expires_at_ms: u64,
    claim_mode: ClaimMode,
) -> CheckpointResult<()> {
    if claim_token.trim().is_empty() || lease_expires_at_ms > MAX_WIRE_INTEGER {
        return Err(CheckpointError::new(
            "checkpoint_claim_invalid",
            "claim token and lease must be non-empty and JSON-safe",
        ));
    }
    checkpoint.revision = checkpoint
        .revision
        .checked_add(1)
        .ok_or_else(|| CheckpointError::new("checkpoint_revision_overflow", "revision overflow"))?;
    if claim_mode == ClaimMode::Recovery {
        checkpoint.resume_attempt = checkpoint.resume_attempt.checked_add(1).ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_resume_attempt_invalid",
                "resume_attempt overflow",
            )
        })?;
    }
    checkpoint.status = CheckpointStatus::Running;
    checkpoint.claim_token = Some(claim_token.to_string());
    checkpoint.claimed_cycle = Some(cycle_index);
    checkpoint.lease_expires_at_ms = Some(lease_expires_at_ms);
    Ok(())
}

pub fn claim_matches(
    current: &CheckpointV2,
    snapshot: &CheckpointV2,
    claim_token: &str,
    expected_revision: u64,
) -> bool {
    current.revision == expected_revision
        && snapshot.revision == expected_revision
        && current.claim_token.as_deref() == Some(claim_token)
        && current.claimed_cycle == snapshot.claimed_cycle
        && current.checkpoint_key == snapshot.checkpoint_key
        && current.terminal_result.is_none()
        && checkpoint_definition_matches(current, snapshot)
}

pub fn checkpoint_definition_matches(current: &CheckpointV2, snapshot: &CheckpointV2) -> bool {
    current.schema_version == snapshot.schema_version
        && current.run_definition_schema == snapshot.run_definition_schema
        && current.checkpoint_key == snapshot.checkpoint_key
        && current.task_id == snapshot.task_id
        && current.root_run_id == snapshot.root_run_id
        && current.trace_id == snapshot.trace_id
        && current.run_definition_digest == snapshot.run_definition_digest
        && current.run_definition == snapshot.run_definition
        && current.resume_attempt == snapshot.resume_attempt
        && current.terminal_acknowledged == snapshot.terminal_acknowledged
}

pub fn prepare_progress(
    current: &CheckpointV2,
    mut snapshot: CheckpointV2,
    claim_token: &str,
    expected_revision: u64,
) -> CheckpointResult<Option<CheckpointV2>> {
    if !claim_matches(current, &snapshot, claim_token, expected_revision) {
        return Ok(None);
    }
    snapshot.claim_token = current.claim_token.clone();
    snapshot.claimed_cycle = current.claimed_cycle;
    snapshot.lease_expires_at_ms = current.lease_expires_at_ms;
    snapshot.revision = expected_revision
        .checked_add(1)
        .ok_or_else(|| CheckpointError::new("checkpoint_revision_overflow", "revision overflow"))?;
    snapshot.validate()?;
    Ok(Some(snapshot))
}

pub fn prepare_suspend(
    current: &CheckpointV2,
    mut snapshot: CheckpointV2,
    claim_token: &str,
    expected_revision: u64,
) -> CheckpointResult<Option<CheckpointV2>> {
    if !claim_matches(current, &snapshot, claim_token, expected_revision)
        || !snapshot.has_ambiguous_operation()
    {
        return Ok(None);
    }
    snapshot.status = CheckpointStatus::ReconciliationRequired;
    snapshot.claim_token = None;
    snapshot.claimed_cycle = None;
    snapshot.lease_expires_at_ms = None;
    snapshot.revision = expected_revision
        .checked_add(1)
        .ok_or_else(|| CheckpointError::new("checkpoint_revision_overflow", "revision overflow"))?;
    snapshot.validate()?;
    Ok(Some(snapshot))
}

pub fn prepare_commit(
    current: &CheckpointV2,
    mut snapshot: CheckpointV2,
    claim_token: &str,
    expected_revision: u64,
) -> CheckpointResult<Option<CheckpointV2>> {
    if !claim_matches(current, &snapshot, claim_token, expected_revision) {
        return Ok(None);
    }
    let Some(claimed_cycle) = current.claimed_cycle else {
        return Ok(None);
    };
    if snapshot.cycle_index != claimed_cycle {
        return Ok(None);
    }
    snapshot.model_call_journal.clear();
    snapshot.tool_journal.clear();
    snapshot.claim_token = None;
    snapshot.claimed_cycle = None;
    snapshot.lease_expires_at_ms = None;
    snapshot.revision = expected_revision
        .checked_add(1)
        .ok_or_else(|| CheckpointError::new("checkpoint_revision_overflow", "revision overflow"))?;
    snapshot.validate()?;
    Ok(Some(snapshot))
}

pub fn prepare_finalize(
    current: &CheckpointV2,
    snapshot: CheckpointV2,
    expected_revision: u64,
) -> CheckpointResult<Option<CheckpointV2>> {
    if current.revision != expected_revision
        || snapshot.revision != expected_revision
        || !checkpoint_definition_matches(current, &snapshot)
        || current.claim_token.is_some()
        || current.terminal_result.is_some()
    {
        return Ok(None);
    }
    prepare_terminal_snapshot(snapshot, expected_revision).map(Some)
}

pub fn prepare_finalize_claimed(
    current: &CheckpointV2,
    snapshot: CheckpointV2,
    claim_token: &str,
    expected_revision: u64,
) -> CheckpointResult<Option<CheckpointV2>> {
    if !claim_matches(current, &snapshot, claim_token, expected_revision) {
        return Ok(None);
    }
    prepare_terminal_snapshot(snapshot, expected_revision).map(Some)
}

fn prepare_terminal_snapshot(
    mut snapshot: CheckpointV2,
    expected_revision: u64,
) -> CheckpointResult<CheckpointV2> {
    let Some(terminal_result) = snapshot.terminal_result.as_ref() else {
        return Err(CheckpointError::new(
            "checkpoint_terminal_result_required",
            "finalize requires terminal_result",
        ));
    };
    if !snapshot.status.is_terminal() {
        return Err(CheckpointError::new(
            "checkpoint_status_invalid",
            "finalize requires a terminal status",
        ));
    }
    let operator_abort = snapshot.is_operator_abort_terminal();
    if !operator_abort {
        snapshot.model_call_journal.clear();
        snapshot.tool_journal.clear();
    }
    snapshot.claim_token = None;
    snapshot.claimed_cycle = None;
    snapshot.lease_expires_at_ms = None;
    snapshot.revision = expected_revision
        .checked_add(1)
        .ok_or_else(|| CheckpointError::new("checkpoint_revision_overflow", "revision overflow"))?;
    let _ = terminal_result;
    snapshot.validate()?;
    Ok(snapshot)
}

pub fn prepare_event_delivery(
    current: &CheckpointV2,
    claim_token: Option<&str>,
    expected_revision: u64,
    event_id: &str,
    payload_digest: &str,
    cursor: EventCursor,
) -> CheckpointResult<Option<CheckpointV2>> {
    if event_id.trim().is_empty() {
        return Err(CheckpointError::new(
            "checkpoint_event_outbox_invalid",
            "event_id must be non-empty",
        ));
    }
    validate_sha256(payload_digest, "event_outbox.payload_digest")?;
    cursor.validate()?;
    if cursor.last_event_id.as_deref() != Some(event_id) {
        return Err(CheckpointError::new(
            "checkpoint_event_cursor_invalid",
            "event cursor last_event_id must match the delivered event",
        ));
    }
    if current.revision != expected_revision || current.claim_token.as_deref() != claim_token {
        return Ok(None);
    }

    let matching = current
        .event_outbox
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.event_id == event_id)
        .collect::<Vec<_>>();
    if matching.len() != 1 {
        return Ok(None);
    }
    let (index, entry) = matching[0];
    if entry.state != "pending" || entry.payload_digest != payload_digest {
        return Ok(None);
    }

    let cursor_value = serde_json::to_value(&cursor).map_err(|error| {
        CheckpointError::new("checkpoint_event_cursor_invalid", error.to_string())
    })?;
    let mut snapshot = current.clone();
    snapshot.event_outbox[index].state = "delivered".to_string();
    snapshot.event_outbox[index].cursor = Some(cursor_value);
    snapshot.event_cursor = Some(cursor);
    snapshot.revision = expected_revision
        .checked_add(1)
        .ok_or_else(|| CheckpointError::new("checkpoint_revision_overflow", "revision overflow"))?;
    snapshot.validate()?;
    Ok(Some(snapshot))
}

pub fn prepare_ack(
    current: &CheckpointV2,
    expected_revision: u64,
) -> CheckpointResult<Option<CheckpointV2>> {
    if current.revision != expected_revision
        || current.terminal_result.is_none()
        || current.claim_token.is_some()
        || current.terminal_acknowledged
    {
        return Ok(None);
    }
    let mut snapshot = current.clone();
    snapshot.terminal_acknowledged = true;
    snapshot.revision = expected_revision
        .checked_add(1)
        .ok_or_else(|| CheckpointError::new("checkpoint_revision_overflow", "revision overflow"))?;
    snapshot.validate()?;
    Ok(Some(snapshot))
}
