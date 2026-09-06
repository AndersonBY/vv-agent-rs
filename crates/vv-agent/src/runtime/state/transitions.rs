use super::*;
use crate::events::RunEventPayload;
use crate::types::{ToolExecutionResult, ToolResultStatus};

pub fn claim_candidate(
    checkpoint: &Checkpoint,
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
    checkpoint: &mut Checkpoint,
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
    current: &Checkpoint,
    snapshot: &Checkpoint,
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

pub fn checkpoint_definition_matches(current: &Checkpoint, snapshot: &Checkpoint) -> bool {
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
    current: &Checkpoint,
    mut snapshot: Checkpoint,
    claim_token: &str,
    expected_revision: u64,
) -> CheckpointResult<Option<Checkpoint>> {
    if !claim_matches(current, &snapshot, claim_token, expected_revision) {
        return Ok(None);
    }
    snapshot.claim_token = current.claim_token.clone();
    snapshot.claimed_cycle = current.claimed_cycle;
    snapshot.lease_expires_at_ms = current.lease_expires_at_ms;
    merge_authoritative_fields(current, &mut snapshot)?;
    snapshot.revision = expected_revision
        .checked_add(1)
        .ok_or_else(|| CheckpointError::new("checkpoint_revision_overflow", "revision overflow"))?;
    snapshot.validate()?;
    Ok(Some(snapshot))
}

#[allow(clippy::too_many_arguments)]
pub fn prepare_tool_receipt(
    current: &Checkpoint,
    checkpoint: &Checkpoint,
    operation_id: &str,
    attempt: u64,
    tool_call_id: &str,
    request_digest: &str,
    result: &ToolExecutionResult,
    claim_token: &str,
    expected_revision: u64,
    claimed_cycle: u64,
) -> CheckpointResult<Option<Checkpoint>> {
    if claim_token.trim().is_empty()
        || current.claim_token.is_none()
        || checkpoint.claim_token.is_none()
    {
        return Err(CheckpointError::new(
            "checkpoint_claim_required",
            "record_tool_receipt requires an active claim",
        ));
    }
    if current.claim_token.as_deref() != Some(claim_token)
        || checkpoint.claim_token.as_deref() != Some(claim_token)
        || current.claimed_cycle != Some(claimed_cycle)
        || checkpoint.claimed_cycle != Some(claimed_cycle)
    {
        return Err(CheckpointError::new(
            "checkpoint_claim_conflict",
            "record_tool_receipt claim does not match the active owner",
        ));
    }
    if current.revision != expected_revision || checkpoint.revision != expected_revision {
        return Err(CheckpointError::new(
            "checkpoint_revision_conflict",
            "record_tool_receipt expected revision is stale",
        ));
    }
    if current.status != CheckpointStatus::Running
        || current.terminal_result.is_some()
        || !checkpoint_definition_matches(current, checkpoint)
    {
        return Ok(None);
    }
    let Some(index) = current.tool_journal.iter().position(|entry| {
        entry.operation_id == operation_id
            && entry.attempt == attempt
            && entry.tool_call_id.as_deref() == Some(tool_call_id)
            && entry.request_digest == request_digest
            && entry.cycle_index == claimed_cycle
            && entry.state == OperationState::Started
    }) else {
        return Ok(None);
    };
    crate::checkpoint::validate_definitive_result(result)?;
    let identity_key = crate::checkpoint::tool_receipt_identity_key(
        &current.checkpoint_key,
        operation_id,
        attempt,
        tool_call_id,
        request_digest,
    )?;
    let result_digest = crate::checkpoint::tool_result_digest(result)?;
    let mut updated = current.clone();
    let entry = &mut updated.tool_journal[index];
    entry.identity_key = Some(identity_key);
    entry.result_digest = Some(result_digest);
    entry.deferred_handle = None;
    entry.resume_observation = None;
    match result.status {
        ToolResultStatus::Success => {
            entry.state = OperationState::Succeeded;
            entry.result = Some(result.to_dict());
            entry.error = None;
        }
        ToolResultStatus::Error => {
            entry.state = OperationState::Failed;
            entry.result = Some(result.to_dict());
            entry.error = Some(operation_error_from_tool_result(result));
            if result.error_code.as_deref() == Some("tool_outcome_unknown") {
                entry.resume_observation = Some(unknown_tool_observation(entry));
            }
        }
        _ => unreachable!("definitive result validated"),
    }
    entry.validate()?;
    let event_entry = entry.clone();
    let event = crate::runtime::state::receipt_event(&updated, &event_entry, result)?;
    updated.event_outbox.push(event);
    updated.revision = expected_revision
        .checked_add(1)
        .ok_or_else(|| CheckpointError::new("checkpoint_revision_overflow", "revision overflow"))?;
    updated.validate()?;
    Ok(Some(updated))
}

pub(crate) fn unknown_tool_observation(entry: &OperationJournalEntry) -> ResumeObservation {
    ResumeObservation {
        operation_id: entry.operation_id.clone(),
        operation_kind: OperationKind::Tool,
        cycle_index: entry.cycle_index,
        state: OperationState::Ambiguous,
        risk: "unknown_tool_side_effect".to_string(),
        idempotency_support: entry.idempotency_support,
    }
}

pub(crate) fn operation_error_from_tool_result(result: &ToolExecutionResult) -> OperationError {
    OperationError::new(
        result
            .error_code
            .as_deref()
            .filter(|code| !code.is_empty())
            .unwrap_or("tool_operation_failed")
            .to_string(),
        if result.content.is_empty() {
            "tool operation failed".to_string()
        } else {
            result.content.clone()
        },
        result
            .metadata
            .get("retryable")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    )
}

pub fn prepare_suspend(
    current: &Checkpoint,
    mut snapshot: Checkpoint,
    claim_token: &str,
    expected_revision: u64,
) -> CheckpointResult<Option<Checkpoint>> {
    if !claim_matches(current, &snapshot, claim_token, expected_revision)
        || !snapshot.has_ambiguous_operation()
    {
        return Ok(None);
    }
    merge_authoritative_fields(current, &mut snapshot)?;
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
    current: &Checkpoint,
    mut snapshot: Checkpoint,
    claim_token: &str,
    expected_revision: u64,
) -> CheckpointResult<Option<Checkpoint>> {
    if !claim_matches(current, &snapshot, claim_token, expected_revision) {
        return Ok(None);
    }
    if current.cancel_requested || snapshot.cancel_requested {
        return Err(CheckpointError::new(
            "checkpoint_cycle_cancel_requested",
            "cancelled cycles must use claimed terminal finalization",
        ));
    }
    let Some(claimed_cycle) = current.claimed_cycle else {
        return Ok(None);
    };
    if snapshot.cycle_index != claimed_cycle {
        return Ok(None);
    }
    if snapshot
        .model_call_journal
        .iter()
        .chain(snapshot.tool_journal.iter())
        .any(|entry| {
            matches!(
                entry.state,
                OperationState::Started | OperationState::Deferred | OperationState::Ambiguous
            )
        })
    {
        return Err(CheckpointError::new(
            "checkpoint_cycle_unresolved",
            "cycle commit requires all operation journal entries to be closed",
        ));
    }
    validate_model_journal_accounting(&snapshot)?;
    snapshot
        .event_outbox
        .retain(|entry| entry.state == "pending");
    snapshot.model_call_journal.clear();
    snapshot.tool_journal.clear();
    snapshot.cancel_requested = false;
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
    current: &Checkpoint,
    mut snapshot: Checkpoint,
    expected_revision: u64,
) -> CheckpointResult<Option<Checkpoint>> {
    if current.revision != expected_revision
        || snapshot.revision != expected_revision
        || !checkpoint_definition_matches(current, &snapshot)
        || current.claim_token.is_some()
        || current.terminal_result.is_some()
    {
        return Ok(None);
    }
    merge_authoritative_fields(current, &mut snapshot)?;
    prepare_terminal_snapshot(snapshot, expected_revision).map(Some)
}

pub fn prepare_finalize_claimed(
    current: &Checkpoint,
    mut snapshot: Checkpoint,
    claim_token: &str,
    expected_revision: u64,
) -> CheckpointResult<Option<Checkpoint>> {
    if !claim_matches(current, &snapshot, claim_token, expected_revision) {
        return Ok(None);
    }
    merge_authoritative_fields(current, &mut snapshot)?;
    prepare_terminal_snapshot(snapshot, expected_revision).map(Some)
}

fn merge_authoritative_fields(
    current: &Checkpoint,
    snapshot: &mut Checkpoint,
) -> CheckpointResult<()> {
    snapshot.cancel_requested |= current.cancel_requested;
    let mut event_outbox = current.event_outbox.clone();
    for candidate in snapshot.event_outbox.drain(..) {
        crate::runtime::state::append_event_outbox_once(&mut event_outbox, candidate)?;
    }
    snapshot.event_outbox = event_outbox;
    snapshot.event_cursor = current.event_cursor.clone();
    Ok(())
}

fn prepare_terminal_snapshot(
    mut snapshot: Checkpoint,
    expected_revision: u64,
) -> CheckpointResult<Checkpoint> {
    if snapshot.terminal_result.is_none() {
        return Err(CheckpointError::new(
            "checkpoint_terminal_result_required",
            "finalize requires terminal_result",
        ));
    }
    if !snapshot.status.is_terminal() {
        return Err(CheckpointError::new(
            "checkpoint_status_invalid",
            "finalize requires a terminal status",
        ));
    }
    let closure_reason = terminal_closure_reason(&snapshot);
    if let Some(reason) = closure_reason {
        if let Some(claimed_cycle) = snapshot.claimed_cycle {
            snapshot.cycle_index = claimed_cycle.saturating_sub(1);
        }
        close_unresolved_tools(&mut snapshot, reason)?;
        snapshot.model_call_journal.clear();
    }
    validate_model_journal_accounting(&snapshot)?;
    snapshot.event_outbox.retain(|entry| {
        entry.state == "pending"
            || matches!(
                entry.event.get("type").and_then(Value::as_str),
                Some("tool_call_completed")
                    | Some("run_state_changed")
                    | Some("run_cancelled")
                    | Some("run_failed")
                    | Some("cycle_aborted")
            )
    });
    if closure_reason.is_none() {
        snapshot.model_call_journal.clear();
        snapshot.tool_journal.clear();
    }
    snapshot.claim_token = None;
    snapshot.claimed_cycle = None;
    snapshot.lease_expires_at_ms = None;
    snapshot.revision = expected_revision
        .checked_add(1)
        .ok_or_else(|| CheckpointError::new("checkpoint_revision_overflow", "revision overflow"))?;
    snapshot.validate()?;
    Ok(snapshot)
}

fn terminal_closure_reason(checkpoint: &Checkpoint) -> Option<&'static str> {
    let result = checkpoint.terminal_result.as_ref()?.as_object()?;
    let error_code = result
        .get("error")
        .and_then(Value::as_object)
        .and_then(|error| error.get("code"))
        .and_then(Value::as_str);
    match error_code {
        Some("operator_abort_with_unknown_outcome") => Some("operator_abort"),
        Some("cancelled") | Some("cancelled_with_unknown_outcome") => Some("cancelled"),
        Some("checkpoint_lease_lost")
        | Some("lease_lost")
        | Some("lease_lost_with_unknown_outcome") => Some("lease_lost"),
        _ => None,
    }
}

pub(crate) fn close_unresolved_tools(
    checkpoint: &mut Checkpoint,
    reason: &str,
) -> CheckpointResult<()> {
    let mut observations = checkpoint
        .terminal_result
        .as_ref()
        .and_then(|result| result.get("resume_observations"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut closed_operation_ids = std::collections::BTreeSet::new();
    let mut closure_observations = Vec::new();
    let logical_cycle = checkpoint
        .claimed_cycle
        .or_else(|| checkpoint.cycle_index.checked_add(1))
        .ok_or_else(|| {
            CheckpointError::new("checkpoint_cycle_invalid", "logical cycle overflow")
        })?;
    if logical_cycle == 0 || logical_cycle > MAX_WIRE_INTEGER {
        return Err(CheckpointError::new(
            "checkpoint_cycle_invalid",
            "logical cycle is outside the JSON-safe range",
        ));
    }
    let mut closed_any = false;
    for entry in &mut checkpoint.tool_journal {
        if !matches!(
            entry.state,
            OperationState::Planned
                | OperationState::Started
                | OperationState::Deferred
                | OperationState::Ambiguous
        ) {
            continue;
        }
        let observation = ResumeObservation {
            operation_id: entry.operation_id.clone(),
            operation_kind: OperationKind::Tool,
            cycle_index: entry.cycle_index,
            state: OperationState::Ambiguous,
            risk: "unknown_tool_side_effect".to_string(),
            idempotency_support: entry.idempotency_support,
        };
        closed_operation_ids.insert((
            entry.operation_id.clone(),
            observation_kind_rank(OperationKind::Tool),
            entry.cycle_index,
        ));
        closure_observations.push(
            serde_json::to_value(&observation).map_err(|error| {
                CheckpointError::new("checkpoint_json_invalid", error.to_string())
            })?,
        );
        let identity = crate::checkpoint::tool_receipt_identity_key(
            &checkpoint.checkpoint_key,
            &entry.operation_id,
            entry.attempt,
            entry.tool_call_id.as_deref().unwrap_or_default(),
            &entry.request_digest,
        )?;
        entry.identity_key = Some(identity);
        entry.result_digest = None;
        entry.resume_observation = Some(observation);
        entry.deferred_handle = None;
        entry.state = OperationState::Failed;
        entry.result = None;
        entry.error = Some(OperationError::new(
            "tool_cancelled",
            "Tool execution ended before a definitive receipt; external effect remains unknown.",
            false,
        ));
        entry.validate()?;
        closed_any = true;
    }
    let has_closure_evidence = closed_any || !observations.is_empty();
    if has_closure_evidence {
        let event_cycle = u32::try_from(logical_cycle.saturating_sub(1)).map_err(|_| {
            CheckpointError::new("checkpoint_cycle_invalid", "cycle event cycle is too large")
        })?;
        let cycle_event = RunEvent::new(
            checkpoint.root_run_id.clone(),
            checkpoint.trace_id.clone(),
            checkpoint.task_id.clone(),
            Some(event_cycle),
            RunEventPayload::CycleAborted {
                logical_cycle,
                reason: reason.to_string(),
            },
        )
        .with_event_id(format!("evt_cycle_aborted_{reason}"))
        .map_err(|error| CheckpointError::new("checkpoint_event_invalid", error))?;
        let event_value = serde_json::to_value(cycle_event)
            .map_err(|error| CheckpointError::new("checkpoint_event_invalid", error.to_string()))?;
        append_event_once(
            checkpoint,
            format!("evt_cycle_aborted_{reason}"),
            event_value,
        )?;
    }
    if let Some(result) = checkpoint
        .terminal_result
        .as_mut()
        .and_then(Value::as_object_mut)
    {
        observations.retain(|observation| {
            observation_key(observation).is_none_or(|key| !closed_operation_ids.contains(&key))
        });
        observations.extend(closure_observations);
        observations.sort_by_key(observation_key);
        observations.dedup_by(|left, right| observation_key(left) == observation_key(right));
        result.insert(
            "resume_observations".to_string(),
            Value::Array(observations),
        );
    }
    Ok(())
}

fn observation_kind_rank(kind: OperationKind) -> u8 {
    match kind {
        OperationKind::Model => 0,
        OperationKind::Tool => 1,
    }
}

fn observation_key(value: &Value) -> Option<(String, u8, u64)> {
    let operation_id = value.get("operation_id")?.as_str()?.to_string();
    let operation_kind = match value.get("operation_kind")?.as_str()? {
        "model" => 0,
        "tool" => 1,
        _ => return None,
    };
    let cycle_index = value.get("cycle_index")?.as_u64()?;
    Some((operation_id, operation_kind, cycle_index))
}

fn append_event_once(
    checkpoint: &mut Checkpoint,
    event_id: String,
    event: Value,
) -> CheckpointResult<()> {
    let entry = EventOutboxEntry::pending(event_id, event)?;
    let original_len = checkpoint.event_outbox.len();
    crate::runtime::state::append_event_outbox_once(&mut checkpoint.event_outbox, entry)?;
    if checkpoint.event_outbox.len() == original_len {
        return Ok(());
    }
    let entry = checkpoint
        .event_outbox
        .pop()
        .expect("append_event_outbox_once appended an entry");
    let terminal_event_index = checkpoint.event_outbox.iter().position(|entry| {
        matches!(
            entry.event.get("type").and_then(Value::as_str),
            Some("run_cancelled") | Some("run_completed") | Some("run_failed")
        ) || (entry.event.get("type").and_then(Value::as_str) == Some("run_state_changed")
            && entry
                .event
                .get("state")
                .and_then(Value::as_str)
                .is_some_and(|state| state != "running"))
    });
    if let Some(index) = terminal_event_index {
        checkpoint.event_outbox.insert(index, entry);
    } else {
        checkpoint.event_outbox.push(entry);
    }
    Ok(())
}

pub fn prepare_event_delivery(
    current: &Checkpoint,
    claim_token: Option<&str>,
    expected_revision: u64,
    event_id: &str,
    payload_digest: &str,
    cursor: EventCursor,
) -> CheckpointResult<Option<Checkpoint>> {
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
    current: &Checkpoint,
    expected_revision: u64,
) -> CheckpointResult<Option<Checkpoint>> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycle_aborted_replay_reuses_the_original_created_at() {
        let mut checkpoint = Checkpoint {
            checkpoint_key: "checkpoint-cycle-replay".to_string(),
            task_id: "task-cycle-replay".to_string(),
            root_run_id: "run-cycle-replay".to_string(),
            trace_id: "trace-cycle-replay".to_string(),
            cycle_index: 1,
            terminal_result: Some(serde_json::json!({"resume_observations": []})),
            ..Checkpoint::default()
        };
        checkpoint.tool_journal.push(OperationJournalEntry::tool(
            "operation-cycle-replay",
            2,
            1,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "call-cycle-replay",
            "tool",
            Map::new(),
            Some("idem-cycle-replay".to_string()),
            ToolIdempotency::Unknown,
        ));

        close_unresolved_tools(&mut checkpoint, "cancelled").expect("first closure");
        let original_event = checkpoint.event_outbox[0].event.clone();
        close_unresolved_tools(&mut checkpoint, "cancelled").expect("same-id replay");

        assert_eq!(checkpoint.event_outbox.len(), 1);
        assert_eq!(checkpoint.event_outbox[0].event, original_event);
    }
}
