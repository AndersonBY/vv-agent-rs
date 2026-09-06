use redis::Commands;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::tempdir;

use vv_agent::checkpoint::controller_receipt_outbox_id;
use vv_agent::runtime::checkpoint_codec::checkpoint_from_value;
use vv_agent::{
    derive_controller_command_id, CheckpointStore, ClaimMode, ControllerCommand,
    ControllerCommandResolution, ControllerCommandVariant, ControllerCommandWakeRecord,
    ControllerHandle, HostInteractionAdmissionContext, HostInteractionMessage,
    HostInteractionRecord, HostInteractionRecoveryEnvelope, HostInteractionRequest,
    HostInteractionResponse, InMemoryCheckpointStore, NotificationOutboxState,
    RedisCheckpointStore, SqliteCheckpointStore,
};

const CODEC_FIXTURE: &str = include_str!("fixtures/parity/checkpoint_codec.json");

#[path = "controller_command/cancel.rs"]
mod controller_command_cancel;
#[path = "controller_command/notification_abort.rs"]
mod controller_command_notification_abort;
#[path = "controller_command/redis.rs"]
mod controller_command_redis;
#[path = "controller_command/strict.rs"]
mod controller_command_strict;
#[path = "controller_command/wake_reaper.rs"]
mod controller_command_wake_reaper;

#[test]
fn app_server_command_id_matches_contract_golden() {
    assert_eq!(
        derive_controller_command_id("thread-1", "turn-1", "same-action").expect("command id"),
        "48d6ee2d2a12b910a61370db73c06835bfe3946258bff4eff1cfd6739bd5be9a"
    );
}

fn minimal_checkpoint() -> vv_agent::Checkpoint {
    let mut fixture: Value = serde_json::from_str(CODEC_FIXTURE).expect("codec fixture");
    let payload = fixture["valid_cases"]
        .as_array_mut()
        .expect("valid cases")
        .iter_mut()
        .find(|case| case["name"] == "minimal_running")
        .expect("minimal running case")["payload"]
        .clone();
    let mut payload = payload;
    payload["checkpoint_key"] = json!("checkpoint-controller-test");
    checkpoint_from_value(&payload, 262_144).expect("valid checkpoint")
}

fn reconciliation_checkpoint(key: &str) -> vv_agent::Checkpoint {
    let fixture: Value = serde_json::from_str(CODEC_FIXTURE).expect("codec fixture");
    let payload = fixture["valid_cases"]
        .as_array()
        .expect("valid cases")
        .iter()
        .find(|case| case["name"] == "reconciliation_required_retains_ambiguous_journal")
        .expect("reconciliation fixture")["payload"]
        .clone();
    let mut payload = payload;
    payload["checkpoint_key"] = json!(key);
    checkpoint_from_value(&payload, 262_144).expect("valid reconciliation checkpoint")
}

fn create_reconciliation_checkpoint(store: &dyn CheckpointStore, target: vv_agent::Checkpoint) {
    let key = target.checkpoint_key.clone();
    let journal = target.tool_journal.clone();
    let mut seed = target;
    seed.revision = 0;
    seed.resume_attempt = 1;
    seed.cycle_index = 0;
    seed.status = vv_agent::CheckpointStatus::Running;
    seed.cancel_requested = false;
    seed.active_host_interaction = None;
    seed.suspended_origin = None;
    seed.cycles.clear();
    seed.model_calls.clear();
    seed.event_cursor = None;
    seed.event_outbox.clear();
    seed.model_call_journal.clear();
    seed.tool_journal.clear();
    seed.claim_token = None;
    seed.claimed_cycle = None;
    seed.lease_expires_at_ms = None;
    seed.terminal_result = None;
    seed.terminal_acknowledged = false;
    assert!(store.create_checkpoint(seed).expect("create seed"));

    let claimed = store
        .claim_checkpoint(
            &key,
            1,
            "reconciliation-owner",
            400,
            300,
            ClaimMode::Continue,
        )
        .expect("claim first cycle")
        .expect("first cycle claim");
    assert!(store
        .progress_checkpoint(claimed.clone(), "reconciliation-owner", claimed.revision,)
        .expect("progress first cycle"));
    let mut committed = store
        .load_checkpoint(&key)
        .expect("load progressed checkpoint")
        .expect("progressed checkpoint");
    committed.cycle_index = 1;
    assert!(store
        .commit_checkpoint(committed, "reconciliation-owner", claimed.revision + 1)
        .expect("commit first cycle"));

    let mut recovered = store
        .claim_checkpoint(
            &key,
            2,
            "reconciliation-owner-2",
            600,
            500,
            ClaimMode::Recovery,
        )
        .expect("claim recovery cycle")
        .expect("recovery cycle claim");
    recovered.tool_journal = journal;
    assert!(store
        .suspend_checkpoint(recovered, "reconciliation-owner-2", claimed.revision + 3,)
        .expect("suspend reconciliation checkpoint"));
}

fn assert_active_wake_replay_is_zero_write(store: &dyn CheckpointStore, command_id: &str) {
    let checkpoint = minimal_checkpoint();
    let key = checkpoint.checkpoint_key.clone();
    let run_id = checkpoint.root_run_id.clone();
    let trace_id = checkpoint.trace_id.clone();
    assert!(store
        .create_checkpoint(checkpoint)
        .expect("create checkpoint"));
    let claimed = store
        .claim_checkpoint(&key, 1, "execution-owner", 100, 0, ClaimMode::Continue)
        .expect("claim checkpoint")
        .expect("claimed checkpoint");
    let request = HostInteractionRequest::new(
        format!("{command_id}-interaction"),
        1,
        format!("{command_id}-operation"),
        format!("{command_id}-tool"),
        "Choose one.",
    )
    .expect("request");
    let admission =
        HostInteractionAdmissionContext::new(&key, claimed.revision, "execution-owner", 1, 0, 100)
            .expect("admission");
    let outcome = store
        .produce_host_interaction(request.clone(), &admission)
        .expect("produce host interaction");
    let command = ControllerCommand::new(
        command_id,
        ControllerHandle::new(&key, &run_id, &trace_id).expect("handle"),
        1,
        outcome.checkpoint_revision,
        ControllerCommandVariant::HostInteractionResponse {
            interaction_id: request.interaction_id.clone(),
            logical_cycle: request.logical_cycle,
            operation_id: request.operation_id.clone(),
            tool_call_id: request.tool_call_id.clone(),
            request_digest: request.request_digest.clone(),
            response: HostInteractionMessage::user("approved").expect("response"),
        },
    )
    .expect("command");
    let receipt = store
        .admit_controller_command(command.clone())
        .expect("admit command");
    assert_eq!(receipt.outbox_state, "pending");
    let claimed = store
        .claim_controller_command_wake(
            &command.command_id,
            &command.command_digest,
            "wake-owner-a",
            100,
            1,
        )
        .expect("claim wake")
        .expect("claimed wake");
    assert_eq!(claimed.outbox_attempt, 1);
    assert_eq!(
        store
            .claim_controller_command_wake(
                &command.command_id,
                &command.command_digest,
                "wake-owner-a",
                200,
                2,
            )
            .expect("same-owner replay")
            .expect("same-owner receipt"),
        claimed
    );
    assert!(store
        .claim_controller_command_wake(
            &command.command_id,
            &command.command_digest,
            "wake-owner-b",
            300,
            2,
        )
        .is_err());
    assert_eq!(
        store
            .get_controller_command_receipt(&command.command_id)
            .expect("receipt")
            .expect("stored receipt")
            .outbox_attempt,
        1
    );
}

#[test]
fn active_wake_same_owner_replay_is_zero_write_memory_and_sqlite() {
    let memory = InMemoryCheckpointStore::new();
    assert_active_wake_replay_is_zero_write(&memory, "memory-active-wake");
    let directory = tempdir().expect("tempdir");
    let sqlite =
        SqliteCheckpointStore::new(directory.path().join("checkpoint.sqlite")).expect("sqlite");
    assert_active_wake_replay_is_zero_write(&sqlite, "sqlite-active-wake");
}

#[test]
fn sqlite_checkpoint_upsert_preserves_cancel_requested() {
    let directory = tempdir().expect("tempdir");
    let store =
        SqliteCheckpointStore::new(directory.path().join("checkpoint.sqlite")).expect("sqlite");
    let mut checkpoint = minimal_checkpoint();
    store
        .save_checkpoint(checkpoint.clone())
        .expect("initial upsert");
    checkpoint.cancel_requested = true;
    store.save_checkpoint(checkpoint).expect("cancel upsert");
    assert!(
        store
            .load_checkpoint("checkpoint-controller-test")
            .expect("load")
            .expect("checkpoint")
            .cancel_requested
    );
}

fn admission_context(
    store: &dyn CheckpointStore,
    checkpoint_key: &str,
    claim_token: &str,
    now_ms: u64,
) -> HostInteractionAdmissionContext {
    let checkpoint = store
        .load_checkpoint(checkpoint_key)
        .expect("load claimed checkpoint")
        .expect("claimed checkpoint");
    HostInteractionAdmissionContext::new(
        checkpoint_key,
        checkpoint.revision,
        claim_token,
        checkpoint.claimed_cycle.expect("claimed cycle"),
        now_ms,
        checkpoint.lease_expires_at_ms.expect("claim lease"),
    )
    .expect("admission context")
}

#[test]
fn host_interaction_request_uses_contract_digest_vector() {
    let request = HostInteractionRequest::new(
        "interaction-42",
        4,
        "op_host_cycle_4",
        "call_host_4",
        "Choose an approved option.",
    )
    .expect("request");
    assert_eq!(
        request.request_digest,
        "6eb7f7953c3aaa93c94dfe723ffa00aecb877505af29f2730b9950c20961c787"
    );
    assert_eq!(
        request.to_value_without_digest(),
        json!({
            "interaction_id": "interaction-42",
            "logical_cycle": 4,
            "operation_id": "op_host_cycle_4",
            "prompt": "Choose an approved option.",
            "schema_version": "vv-agent.host-interaction-request.v1",
            "tool_call_id": "call_host_4"
        })
    );
}

#[test]
fn producer_rejects_expired_claim_before_writing_memory_or_sqlite() {
    let request = HostInteractionRequest::new(
        "interaction-expired",
        1,
        "operation-expired",
        "tool-expired",
        "Choose.",
    )
    .expect("request");

    let memory = InMemoryCheckpointStore::new();
    let mut checkpoint = minimal_checkpoint();
    checkpoint.checkpoint_key = "checkpoint-expired-memory".to_string();
    let memory_key = checkpoint.checkpoint_key.clone();
    memory.create_checkpoint(checkpoint).expect("create memory");
    let claimed = memory
        .claim_checkpoint(&memory_key, 1, "memory-worker", 100, 0, ClaimMode::Continue)
        .expect("claim memory")
        .expect("memory claim");
    let expired = HostInteractionAdmissionContext::new(
        &memory_key,
        claimed.revision,
        "memory-worker",
        1,
        100,
        100,
    )
    .expect("expired context shape");
    let error = memory
        .produce_host_interaction(request.clone(), &expired)
        .err()
        .expect("expired memory claim");
    assert_eq!(error.code(), "host_interaction_claim_required");
    let unchanged = memory
        .load_checkpoint(&memory_key)
        .expect("load memory")
        .expect("memory checkpoint");
    assert_eq!(unchanged.status, vv_agent::CheckpointStatus::Running);
    assert_eq!(unchanged.revision, claimed.revision);
    assert_eq!(unchanged.claim_token.as_deref(), Some("memory-worker"));

    let directory = tempdir().expect("tempdir");
    let path = directory.path().join("expired.sqlite");
    let sqlite = SqliteCheckpointStore::new(&path).expect("open sqlite");
    let mut checkpoint = minimal_checkpoint();
    checkpoint.checkpoint_key = "checkpoint-expired-sqlite".to_string();
    let sqlite_key = checkpoint.checkpoint_key.clone();
    sqlite.create_checkpoint(checkpoint).expect("create sqlite");
    let claimed = sqlite
        .claim_checkpoint(&sqlite_key, 1, "sqlite-worker", 100, 0, ClaimMode::Continue)
        .expect("claim sqlite")
        .expect("sqlite claim");
    let expired = HostInteractionAdmissionContext::new(
        &sqlite_key,
        claimed.revision,
        "sqlite-worker",
        1,
        100,
        100,
    )
    .expect("expired context shape");
    let error = sqlite
        .produce_host_interaction(request, &expired)
        .err()
        .expect("expired sqlite claim");
    assert_eq!(error.code(), "host_interaction_claim_required");
    let unchanged = sqlite
        .load_checkpoint(&sqlite_key)
        .expect("load sqlite")
        .expect("sqlite checkpoint");
    assert_eq!(unchanged.status, vv_agent::CheckpointStatus::Running);
    assert_eq!(unchanged.revision, claimed.revision);
    assert_eq!(unchanged.claim_token.as_deref(), Some("sqlite-worker"));
}

#[test]
fn sqlite_reaper_cas_requires_matching_expired_checkpoint_claim() {
    let directory = tempdir().expect("tempdir");
    let path = directory.path().join("reaper.sqlite");
    let store = SqliteCheckpointStore::new(&path).expect("open sqlite");
    let mut checkpoint = minimal_checkpoint();
    checkpoint.checkpoint_key = "checkpoint-reaper-cas".to_string();
    let key = checkpoint.checkpoint_key.clone();
    store.create_checkpoint(checkpoint).expect("create");
    let claimed = store
        .claim_checkpoint(&key, 1, "initial-owner", 100, 0, ClaimMode::Continue)
        .expect("initial claim")
        .expect("initially claimed");
    let request = HostInteractionRequest::new(
        "interaction-reaper-cas",
        1,
        "operation-reaper-cas",
        "tool-reaper-cas",
        "Choose.",
    )
    .expect("request");
    let admission =
        HostInteractionAdmissionContext::new(&key, claimed.revision, "initial-owner", 1, 0, 100)
            .expect("admission");
    let admitted = store
        .produce_host_interaction(request.clone(), &admission)
        .expect("host admission");
    let current = store
        .load_checkpoint(&key)
        .expect("load after admission")
        .expect("checkpoint after admission");
    let command = ControllerCommand::new(
        "command-reaper-cas",
        ControllerHandle::new(&key, &current.root_run_id, &current.trace_id).expect("handle"),
        current.resume_attempt,
        admitted.checkpoint_revision,
        ControllerCommandVariant::HostInteractionResponse {
            interaction_id: request.interaction_id.clone(),
            logical_cycle: request.logical_cycle,
            operation_id: request.operation_id.clone(),
            tool_call_id: request.tool_call_id.clone(),
            request_digest: request.request_digest.clone(),
            response: HostInteractionMessage::user("approved").expect("response"),
        },
    )
    .expect("response command");
    store
        .resolve_controller_command(command)
        .expect("admit response");
    let current = store
        .load_checkpoint(&key)
        .expect("load running checkpoint")
        .expect("running checkpoint");
    let error = store
        .claim_checkpoint(
            &key,
            current.cycle_index + 1,
            "reaper-owner",
            200,
            0,
            ClaimMode::Continue,
        )
        .expect_err("ordinary claim must stop at host recovery barrier");
    assert_eq!(error.code(), "host_interaction_recovery_required");
    let unchanged = store
        .load_checkpoint(&key)
        .expect("load after rejected claim")
        .expect("checkpoint after rejected claim");
    assert_eq!(unchanged, current);

    let connection = rusqlite::Connection::open(&path).expect("open raw sqlite connection");
    connection
        .execute(
            "UPDATE checkpoints SET claim_token = ?1, claimed_cycle = ?2, lease_expires_at_ms = ?3 WHERE checkpoint_key = ?4",
            rusqlite::params![
                "reaper-owner",
                (current.cycle_index + 1) as i64,
                200_i64,
                key
            ],
        )
        .expect("stage matching checkpoint claim");
    connection
        .execute(
            "UPDATE host_interaction_records SET state = 'resolved_claimed', claim_token = ?1, lease_expires_at_ms = ?2 WHERE record_id = ?3 AND checkpoint_key = ?4",
            rusqlite::params!["different-owner", 200_i64, admitted.record_id, key],
        )
        .expect("stage stale record claim");
    let before_claimed_barrier = store
        .load_checkpoint(&key)
        .expect("load before resolved-claimed barrier")
        .expect("checkpoint before resolved-claimed barrier");
    let error = store
        .claim_checkpoint(
            &key,
            current.cycle_index + 1,
            "another-owner",
            300,
            0,
            ClaimMode::Continue,
        )
        .expect_err("resolved claimed record must stop ordinary claim");
    assert_eq!(error.code(), "host_interaction_recovery_required");
    assert_eq!(
        store
            .load_checkpoint(&key)
            .expect("load after resolved-claimed barrier")
            .expect("checkpoint after resolved-claimed barrier"),
        before_claimed_barrier
    );
    assert!(!store
        .reap_host_interaction_record(&admitted.record_id, &key, 201)
        .expect("stale reaper"));
    connection
        .execute(
            "UPDATE host_interaction_records SET claim_token = ?1 WHERE record_id = ?2 AND checkpoint_key = ?3",
            rusqlite::params!["reaper-owner", admitted.record_id, key],
        )
        .expect("stage matching record claim");
    assert!(store
        .reap_host_interaction_record(&admitted.record_id, &key, 201)
        .expect("matching expired reaper"));
    let (state, claim_token, lease, last_error): (
        String,
        Option<String>,
        Option<i64>,
        Option<String>,
    ) = connection
        .query_row(
            "SELECT state, claim_token, lease_expires_at_ms, last_error FROM host_interaction_records WHERE record_id = ?1 AND checkpoint_key = ?2",
            rusqlite::params![admitted.record_id, key],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("read reaped record");
    assert_eq!(state, "resolved_pending");
    assert_eq!(claim_token, None);
    assert_eq!(lease, None);
    assert_eq!(
        last_error.as_deref(),
        Some("host_interaction_response_claim_expired")
    );
}

#[test]
fn abort_reconciliation_is_terminal_and_emits_ordered_control_events() {
    let checkpoint = reconciliation_checkpoint("checkpoint-controller-abort-memory");
    let key = checkpoint.checkpoint_key.clone();
    let handle =
        ControllerHandle::new(&key, &checkpoint.root_run_id, &checkpoint.trace_id).expect("handle");
    let store = InMemoryCheckpointStore::new();
    create_reconciliation_checkpoint(&store, checkpoint);
    let command = ControllerCommand::new(
        "command-abort-memory",
        handle,
        2,
        5,
        ControllerCommandVariant::Abort,
    )
    .expect("abort command");
    let resolution = store
        .resolve_controller_command(command)
        .expect("abort reconciliation command");
    assert!(matches!(
        resolution,
        ControllerCommandResolution::Applied { ref wake, .. } if wake.action == "none"
    ));
    let checkpoint = store
        .load_checkpoint(&key)
        .expect("load")
        .expect("checkpoint");
    assert_eq!(checkpoint.status, vv_agent::CheckpointStatus::Failed);
    let terminal = checkpoint
        .terminal_result
        .as_ref()
        .expect("terminal result");
    assert_eq!(
        terminal["completion_reason"],
        json!("failed"),
        "abort keeps an explicit failed completion reason"
    );
    assert_eq!(
        terminal["error"]["code"],
        json!("operator_abort_with_unknown_outcome")
    );
    assert_eq!(
        terminal["error"]["message"],
        json!("Operator accepted that the external outcome is unknown.")
    );
    assert_eq!(checkpoint.event_outbox.len(), 3);
    let events = checkpoint
        .event_outbox
        .iter()
        .map(|entry| entry.event.clone())
        .collect::<Vec<_>>();
    assert_eq!(events[0]["type"], json!("cycle_aborted"));
    assert_eq!(events[1]["type"], json!("run_state_changed"));
    assert_eq!(events[2]["type"], json!("run_failed"));
    assert_eq!(events[0]["cycle_index"], json!(1));
    assert_eq!(events[1]["cycle_index"], json!(1));
    assert_eq!(events[2]["cycle_index"], json!(1));
    assert_ne!(
        checkpoint.event_outbox[1].event_id, checkpoint.event_outbox[2].event_id,
        "control event IDs must be distinct within one command"
    );
    assert_eq!(events[2]["completion_reason"], json!("failed"));
    assert_eq!(
        events[2]["error"],
        json!("failed"),
        "C17 abort uses the public failed error on the event wire"
    );
    assert_eq!(
        events[2]["metadata"]["error_code"],
        json!("operator_abort_with_unknown_outcome")
    );
}

#[test]
fn abort_reconciliation_is_supported_by_sqlite() {
    let directory = tempdir().expect("tempdir");
    let path = directory.path().join("abort.sqlite");
    let store = SqliteCheckpointStore::new(&path).expect("open sqlite");
    let checkpoint = reconciliation_checkpoint("checkpoint-controller-abort-sqlite");
    let key = checkpoint.checkpoint_key.clone();
    let handle =
        ControllerHandle::new(&key, &checkpoint.root_run_id, &checkpoint.trace_id).expect("handle");
    create_reconciliation_checkpoint(&store, checkpoint);
    let command = ControllerCommand::new(
        "command-abort-sqlite",
        handle,
        2,
        5,
        ControllerCommandVariant::Abort,
    )
    .expect("abort command");
    let resolution = store
        .resolve_controller_command(command)
        .expect("abort reconciliation command");
    assert!(matches!(
        resolution,
        ControllerCommandResolution::Applied { ref wake, .. } if wake.action == "none"
    ));
    let checkpoint = store
        .load_checkpoint(&key)
        .expect("load")
        .expect("checkpoint");
    assert_eq!(checkpoint.status, vv_agent::CheckpointStatus::Failed);
    assert_eq!(checkpoint.event_outbox.len(), 3);
    assert_eq!(
        checkpoint.event_outbox[2].event["completion_reason"],
        json!("failed")
    );
}

#[test]
fn memory_store_admits_replays_and_consumes_response_once() {
    let store = InMemoryCheckpointStore::new();
    let checkpoint = minimal_checkpoint();
    let key = checkpoint.checkpoint_key.clone();
    let run_id = checkpoint.root_run_id.clone();
    let trace_id = checkpoint.trace_id.clone();
    assert!(store.create_checkpoint(checkpoint).expect("create"));
    let claimed = store
        .claim_checkpoint(&key, 1, "worker-a", 1_000_000, 0, ClaimMode::Continue)
        .expect("claim")
        .expect("claimed checkpoint");
    assert_eq!(claimed.claimed_cycle, Some(1));
    let admission = HostInteractionAdmissionContext::new(
        &key,
        claimed.revision,
        "worker-a",
        claimed.claimed_cycle.expect("claimed cycle"),
        0,
        claimed.lease_expires_at_ms.expect("claim lease"),
    )
    .expect("admission context");

    let request = HostInteractionRequest::new(
        "interaction-test",
        1,
        "operation-test",
        "tool-call-test",
        "Choose an approved option.",
    )
    .expect("request");
    let admitted = store
        .produce_host_interaction(request.clone(), &admission)
        .expect("admit interaction");
    assert_eq!(admitted.status, "admitted");
    assert_eq!(
        store
            .load_checkpoint(&key)
            .expect("load")
            .expect("checkpoint")
            .status,
        vv_agent::CheckpointStatus::HostInteraction
    );
    let replay = store
        .produce_host_interaction(request.clone(), &admission)
        .expect("replay interaction");
    assert_eq!(replay.status, "replayed");
    assert_eq!(replay.checkpoint_revision, admitted.checkpoint_revision);

    let handle = ControllerHandle::new(&key, &run_id, &trace_id).expect("handle");
    let command = ControllerCommand::new(
        "command-test",
        handle,
        1,
        admitted.checkpoint_revision,
        ControllerCommandVariant::HostInteractionResponse {
            interaction_id: request.interaction_id.clone(),
            logical_cycle: request.logical_cycle,
            operation_id: request.operation_id.clone(),
            tool_call_id: request.tool_call_id.clone(),
            request_digest: request.request_digest.clone(),
            response: HostInteractionMessage::user("approved").expect("response"),
        },
    )
    .expect("command");
    let resolution = store
        .resolve_controller_command(command.clone())
        .expect("command admission");
    let (receipt, wake) = match &resolution {
        ControllerCommandResolution::Applied { receipt, wake } => (receipt, wake),
        other => panic!("unexpected resolution: {other:?}"),
    };
    assert_eq!(receipt.resulting_status, "running");
    assert_eq!(wake.action, "recovery_dispatch");
    assert_eq!(wake.logical_cycle, 1);
    let before_barrier = store
        .load_checkpoint(&key)
        .expect("load before ordinary recovery claim")
        .expect("checkpoint before ordinary recovery claim");
    for (claim_mode, claim_token) in [
        (ClaimMode::Continue, "ordinary-continue"),
        (ClaimMode::Recovery, "ordinary-recovery"),
    ] {
        let error = store
            .claim_checkpoint(&key, 2, claim_token, 1_000_000, 0, claim_mode)
            .expect_err("ordinary claim must stop at host recovery barrier");
        assert_eq!(error.code(), "host_interaction_recovery_required");
        let after_barrier = store
            .load_checkpoint(&key)
            .expect("load after rejected ordinary claim")
            .expect("checkpoint after rejected ordinary claim");
        assert_eq!(after_barrier, before_barrier);
    }
    assert_eq!(
        store
            .resolve_controller_command(command.clone())
            .expect("replayed command")
            .kind(),
        "replayed"
    );

    let envelope = HostInteractionRecoveryEnvelope {
        schema_version: "vv-agent.host-interaction-recovery.v1".to_string(),
        record_id: admitted.record_id.clone(),
        checkpoint_key: key.clone(),
        run_id,
        trace_id,
        claim_mode: "recovery".to_string(),
        resume_attempt: 1,
        expected_revision: admitted.checkpoint_revision + 1,
        logical_cycle: 1,
        interaction_id: request.interaction_id,
        operation_id: request.operation_id,
        tool_call_id: request.tool_call_id,
        request_digest: request.request_digest,
        command_id: "command-test".to_string(),
    };
    let consumed = store
        .claim_and_consume_host_interaction_response(envelope.clone())
        .expect("consume response");
    assert_eq!(consumed.kind, "applied");
    assert_eq!(consumed.injection_count, 1);
    let checkpoint = store
        .load_checkpoint(&key)
        .expect("load after consume")
        .expect("checkpoint");
    assert_eq!(checkpoint.resume_attempt, 2);
    assert_eq!(checkpoint.claimed_cycle, Some(1));
    assert_eq!(
        checkpoint
            .messages
            .last()
            .map(|message| message.content.as_str()),
        Some("approved")
    );

    let replay = store
        .claim_and_consume_host_interaction_response(envelope)
        .expect("replay consume");
    assert_eq!(replay.kind, "replayed");
    assert_eq!(replay.injection_count, 1);
    assert_eq!(
        store
            .load_checkpoint(&key)
            .expect("load replay")
            .expect("checkpoint")
            .messages
            .iter()
            .filter(|message| message.content == "approved")
            .count(),
        1
    );
}

#[test]
fn controller_digest_conflict_and_stale_fence_are_zero_write() {
    let store = InMemoryCheckpointStore::new();
    let checkpoint = minimal_checkpoint();
    let key = checkpoint.checkpoint_key.clone();
    let run_id = checkpoint.root_run_id.clone();
    let trace_id = checkpoint.trace_id.clone();
    store.create_checkpoint(checkpoint).expect("create");
    store
        .claim_checkpoint(&key, 1, "worker-a", 1_000_000, 0, ClaimMode::Continue)
        .expect("claim");
    let request = HostInteractionRequest::new("interaction", 1, "operation", "tool", "prompt")
        .expect("request");
    let admitted = store
        .produce_host_interaction(
            request.clone(),
            &admission_context(&store, &key, "worker-a", 0),
        )
        .expect("admit");
    let handle = ControllerHandle::new(&key, &run_id, &trace_id).expect("handle");
    let command = ControllerCommand::new(
        "command-conflict",
        handle.clone(),
        1,
        admitted.checkpoint_revision,
        ControllerCommandVariant::HostInteractionResponse {
            interaction_id: request.interaction_id.clone(),
            logical_cycle: 1,
            operation_id: request.operation_id.clone(),
            tool_call_id: request.tool_call_id.clone(),
            request_digest: request.request_digest.clone(),
            response: HostInteractionMessage::user("one").expect("response"),
        },
    )
    .expect("command");
    store
        .resolve_controller_command(command.clone())
        .expect("admit");
    let mut different = command.to_value();
    different["command"]["response"]["content"] = json!("two");
    different["command_digest"] = json!(format!("{:x}", Sha256::digest(b"different")));
    let conflict =
        ControllerCommand::from_value(&different).expect_err("conflict wire should fail digest");
    assert_eq!(conflict.code(), "controller_command_digest_invalid");

    let stale = ControllerCommand::new(
        "command-stale",
        handle,
        1,
        admitted.checkpoint_revision,
        ControllerCommandVariant::Suspend,
    )
    .expect("stale command");
    let resolution = store
        .resolve_controller_command(stale)
        .expect("stale fence is a closed rejected resolution");
    assert!(matches!(
        resolution,
        ControllerCommandResolution::Rejected { error }
            if error.starts_with("controller_command_stale:")
    ));
    assert_eq!(
        store
            .load_checkpoint(&key)
            .expect("load")
            .expect("checkpoint")
            .revision,
        admitted.checkpoint_revision + 1
    );
}

#[test]
fn sqlite_store_retains_host_record_and_recovery_across_reopen() {
    let directory = tempdir().expect("tempdir");
    let path = directory.path().join("checkpoint.sqlite");
    let store = SqliteCheckpointStore::new(&path).expect("open sqlite");
    let checkpoint = minimal_checkpoint();
    let key = checkpoint.checkpoint_key.clone();
    let run_id = checkpoint.root_run_id.clone();
    let trace_id = checkpoint.trace_id.clone();
    store.create_checkpoint(checkpoint).expect("create");
    store
        .claim_checkpoint(&key, 1, "worker-a", 1_000_000, 0, ClaimMode::Continue)
        .expect("claim");
    let request = HostInteractionRequest::new(
        "interaction-sqlite",
        1,
        "operation-sqlite",
        "tool-sqlite",
        "prompt",
    )
    .expect("request");
    let admitted = store
        .produce_host_interaction(
            request.clone(),
            &admission_context(&store, &key, "worker-a", 0),
        )
        .expect("admit");
    let command = ControllerCommand::new(
        "command-sqlite",
        ControllerHandle::new(&key, &run_id, &trace_id).expect("handle"),
        1,
        admitted.checkpoint_revision,
        ControllerCommandVariant::HostInteractionResponse {
            interaction_id: request.interaction_id.clone(),
            logical_cycle: 1,
            operation_id: request.operation_id.clone(),
            tool_call_id: request.tool_call_id.clone(),
            request_digest: request.request_digest.clone(),
            response: HostInteractionMessage::user("sqlite-approved").expect("response"),
        },
    )
    .expect("command");
    store
        .resolve_controller_command(command.clone())
        .expect("response admission");
    let before_barrier = store
        .load_checkpoint(&key)
        .expect("load before sqlite recovery barrier")
        .expect("sqlite checkpoint before recovery barrier");
    for (claim_mode, claim_token) in [
        (ClaimMode::Continue, "sqlite-ordinary-continue"),
        (ClaimMode::Recovery, "sqlite-ordinary-recovery"),
    ] {
        let error = store
            .claim_checkpoint(&key, 2, claim_token, 1_000_000, 0, claim_mode)
            .expect_err("ordinary sqlite claim must stop at host recovery barrier");
        assert_eq!(error.code(), "host_interaction_recovery_required");
        let after_barrier = store
            .load_checkpoint(&key)
            .expect("load after rejected sqlite claim")
            .expect("sqlite checkpoint after rejected claim");
        assert_eq!(after_barrier, before_barrier);
    }
    let claimed_wake = store
        .claim_controller_command_wake(
            &command.command_id,
            &command.command_digest,
            "sqlite-wake-owner",
            10_000,
            1,
        )
        .expect("claim sqlite wake")
        .expect("sqlite wake receipt");
    assert_eq!(claimed_wake.outbox_state, "claimed");
    let ambiguous = store
        .complete_controller_command_wake(
            &command.command_id,
            &command.command_digest,
            "sqlite-wake-owner",
            1,
            "ambiguous",
            2,
            Some("sqlite callback token=secret"),
        )
        .expect("complete sqlite wake")
        .expect("sqlite ambiguous receipt");
    assert_eq!(ambiguous.outbox_state, "ambiguous");
    let retried = store
        .reconcile_controller_command_wake(&command.command_id, &command.command_digest, "retry", 3)
        .expect("reconcile sqlite wake")
        .expect("sqlite retried receipt");
    assert_eq!(retried.outbox_state, "pending");
    drop(store);

    let reopened = SqliteCheckpointStore::new(&path).expect("reopen sqlite");
    let envelope = HostInteractionRecoveryEnvelope {
        schema_version: "vv-agent.host-interaction-recovery.v1".to_string(),
        record_id: admitted.record_id,
        checkpoint_key: key.clone(),
        run_id,
        trace_id,
        claim_mode: "recovery".to_string(),
        resume_attempt: 1,
        expected_revision: admitted.checkpoint_revision + 1,
        logical_cycle: 1,
        interaction_id: request.interaction_id,
        operation_id: request.operation_id,
        tool_call_id: request.tool_call_id,
        request_digest: request.request_digest,
        command_id: "command-sqlite".to_string(),
    };
    let consumed = reopened
        .claim_and_consume_host_interaction_response(envelope.clone())
        .expect("consume after reopen");
    assert_eq!(consumed.kind, "applied");
    let replay = reopened
        .claim_and_consume_host_interaction_response(envelope)
        .expect("replay after reopen");
    assert_eq!(replay.kind, "replayed");
    let checkpoint = reopened
        .load_checkpoint(&key)
        .expect("load")
        .expect("checkpoint");
    assert_eq!(
        checkpoint
            .messages
            .last()
            .map(|message| message.content.as_str()),
        Some("sqlite-approved")
    );
}
