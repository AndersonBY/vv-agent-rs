use redis::Commands;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::tempdir;

use vv_agent::runtime::checkpoint_codec::checkpoint_from_value;
use vv_agent::{
    derive_controller_command_id, CheckpointStore, ClaimMode, ControllerCommand,
    ControllerCommandResolution, ControllerCommandVariant, ControllerHandle,
    HostInteractionAdmissionContext, HostInteractionMessage, HostInteractionRecord,
    HostInteractionRecoveryEnvelope, HostInteractionRequest, HostInteractionResponse,
    InMemoryCheckpointStore, NotificationOutboxState, RedisCheckpointStore, SqliteCheckpointStore,
};

const CODEC_FIXTURE: &str = include_str!("fixtures/parity/checkpoint_codec.json");

#[path = "controller_command/notification_abort.rs"]
mod controller_command_notification_abort;
#[path = "controller_command/redis.rs"]
mod controller_command_redis;
#[path = "controller_command/strict.rs"]
mod controller_command_strict;

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
    let claimed = store
        .claim_checkpoint(
            &key,
            current.cycle_index + 1,
            "reaper-owner",
            200,
            0,
            ClaimMode::Continue,
        )
        .expect("execution claim")
        .expect("claimed execution");

    let connection = rusqlite::Connection::open(&path).expect("open raw sqlite connection");
    connection
        .execute(
            "UPDATE host_interaction_records SET state = 'resolved_claimed', claim_token = ?1, lease_expires_at_ms = ?2 WHERE record_id = ?3 AND checkpoint_key = ?4",
            rusqlite::params!["different-owner", 200_i64, admitted.record_id, key],
        )
        .expect("stage stale record claim");
    assert!(!store
        .reap_host_interaction_record(&admitted.record_id, &key, 201)
        .expect("stale reaper"));
    connection
        .execute(
            "UPDATE host_interaction_records SET claim_token = ?1 WHERE record_id = ?2 AND checkpoint_key = ?3",
            rusqlite::params![claimed.claim_token.as_deref(), admitted.record_id, key],
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
    store.create_checkpoint(checkpoint).expect("create");
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
        terminal["error_code"],
        json!("operator_abort_with_unknown_outcome")
    );
    assert_eq!(checkpoint.event_outbox.len(), 2);
    let events = checkpoint
        .event_outbox
        .iter()
        .map(|entry| entry.event.clone())
        .collect::<Vec<_>>();
    assert_eq!(events[0]["type"], json!("run_state_changed"));
    assert_eq!(events[1]["type"], json!("run_failed"));
    assert_eq!(events[0]["cycle_index"], json!(1));
    assert_eq!(events[1]["cycle_index"], json!(1));
    assert_ne!(
        checkpoint.event_outbox[0].event_id, checkpoint.event_outbox[1].event_id,
        "control event IDs must be distinct within one command"
    );
    assert_eq!(events[1]["completion_reason"], json!("failed"));
    assert_eq!(
        events[1]["error"],
        json!("failed"),
        "C17 abort uses the public failed error on the event wire"
    );
    assert_eq!(
        events[1]["metadata"]["error_code"],
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
    store.create_checkpoint(checkpoint).expect("create");
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
    assert_eq!(checkpoint.event_outbox.len(), 2);
    assert_eq!(
        checkpoint.event_outbox[1].event["completion_reason"],
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
#[test]
#[ignore = "requires a Python-seeded Redis v8 fixture and is run as an explicit cross-language probe"]
fn redis_reads_python_seeded_host_receipt_and_notification_rows() {
    let Ok(redis_url) = std::env::var("VV_AGENT_TEST_REDIS_URL") else {
        return;
    };
    let checkpoint_key = std::env::var("VV_AGENT_CROSS_REDIS_CHECKPOINT_KEY")
        .expect("VV_AGENT_CROSS_REDIS_CHECKPOINT_KEY");
    let interaction_id = std::env::var("VV_AGENT_CROSS_REDIS_INTERACTION_ID")
        .expect("VV_AGENT_CROSS_REDIS_INTERACTION_ID");
    let notification_id = std::env::var("VV_AGENT_CROSS_REDIS_NOTIFICATION_ID")
        .expect("VV_AGENT_CROSS_REDIS_NOTIFICATION_ID");
    let command_id =
        std::env::var("VV_AGENT_CROSS_REDIS_COMMAND_ID").expect("VV_AGENT_CROSS_REDIS_COMMAND_ID");
    let expected_request_digest = std::env::var("VV_AGENT_CROSS_REDIS_REQUEST_DIGEST")
        .expect("VV_AGENT_CROSS_REDIS_REQUEST_DIGEST");
    let expected_notification_digest = std::env::var("VV_AGENT_CROSS_REDIS_NOTIFICATION_DIGEST")
        .expect("VV_AGENT_CROSS_REDIS_NOTIFICATION_DIGEST");

    let store = RedisCheckpointStore::new(&redis_url).expect("redis");
    let checkpoint = store
        .load_checkpoint(&checkpoint_key)
        .expect("load Python checkpoint")
        .expect("Python checkpoint exists");
    assert_eq!(checkpoint.checkpoint_key, checkpoint_key);

    let notification = store
        .get_host_interaction_notification(&notification_id)
        .expect("decode Python notification")
        .expect("Python notification exists");
    assert_eq!(notification.notification_id, notification_id);
    assert_eq!(notification.checkpoint_key, checkpoint_key);
    assert_eq!(notification.payload.interaction_id, interaction_id);
    assert_eq!(notification.payload_digest, expected_notification_digest);
    assert_eq!(notification.payload.prompt, "Cross-language host prompt");

    let receipt = store
        .get_controller_command_receipt(&command_id)
        .expect("decode Python controller receipt")
        .expect("Python controller receipt exists");
    let command = store
        .get_controller_command(&command_id)
        .expect("decode Python controller command")
        .expect("Python controller command exists");
    assert_eq!(receipt.command_id, command.command_id);
    assert_eq!(receipt.command_digest, command.command_digest);
    assert_eq!(command.handle.checkpoint_key, checkpoint_key);
    assert!(matches!(
        command.command,
        ControllerCommandVariant::HostInteractionResponse { .. }
    ));

    let client = redis::Client::open(redis_url.as_str()).expect("redis client");
    let mut connection = client.get_connection().expect("redis connection");
    let record_key = RedisCheckpointStore::host_interaction_key(&checkpoint_key, &interaction_id);
    let record_wire: Value = serde_json::from_str(
        &connection
            .get::<_, String>(&record_key)
            .expect("Python host record wire"),
    )
    .expect("host record JSON");
    let record = HostInteractionRecord::from_value(&record_wire).expect("strict host record");
    assert_eq!(record.checkpoint_key, checkpoint_key);
    assert_eq!(record.interaction_id, interaction_id);
    assert_eq!(record.request_digest, expected_request_digest);

    let notification_key =
        RedisCheckpointStore::host_interaction_notification_key(&notification_id);
    let notification_wire: Value = serde_json::from_str(
        &connection
            .get::<_, String>(&notification_key)
            .expect("Python notification wire"),
    )
    .expect("notification JSON");
    assert_eq!(
        notification_wire["payload_digest"],
        expected_notification_digest
    );

    // Rust owns the next lifecycle transitions; the Python probe reads this
    // same row afterward, proving that notification reconciliation is not a
    // language-local shadow store.
    let claim = store
        .claim_host_interaction_notification(
            &notification_id,
            &expected_notification_digest,
            "cross-rust-owner",
            1_000,
            100,
        )
        .expect("Rust notification claim")
        .expect("claimable Python notification");
    let claim_token = claim.claim_token.as_deref().expect("claim token");
    let ambiguous = store
        .complete_host_interaction_notification(
            &notification_id,
            &expected_notification_digest,
            claim_token,
            claim.attempt,
            "ambiguous",
            101,
            Some("cross-language observer ambiguity"),
        )
        .expect("Rust notification ambiguity")
        .expect("ambiguous notification");
    assert_eq!(ambiguous.outbox_state, NotificationOutboxState::Ambiguous);
    let delivered = store
        .reconcile_host_interaction_notification(
            &notification_id,
            &expected_notification_digest,
            "delivered",
            200,
            None,
        )
        .expect("Rust notification reconciliation")
        .expect("reconciled notification");
    assert_eq!(delivered.outbox_state, NotificationOutboxState::Delivered);
}

#[test]
fn sqlite_public_notification_redacts_all_credential_markers() {
    let directory = tempdir().expect("tempdir");
    let path = directory.path().join("redaction.sqlite");
    let store = SqliteCheckpointStore::new(&path).expect("open sqlite");
    let mut checkpoint = minimal_checkpoint();
    checkpoint.checkpoint_key = "checkpoint-controller-redaction".to_string();
    let key = checkpoint.checkpoint_key.clone();
    store.create_checkpoint(checkpoint).expect("create");
    store
        .claim_checkpoint(
            &key,
            1,
            "redaction-worker",
            1_000_000,
            0,
            ClaimMode::Continue,
        )
        .expect("claim");
    let request = HostInteractionRequest::new(
        "interaction-redaction",
        1,
        "operation-redaction",
        "tool-redaction",
        "Choose normally api_key=secret-api password=hunter2 Authorization: Bearer bearer-secret sk-live-secret token=token-secret at https://example.invalid/run?secret=abc",
    )
    .expect("request");
    store
        .produce_host_interaction(
            request,
            &admission_context(&store, &key, "redaction-worker", 0),
        )
        .expect("produce");
    let connection = rusqlite::Connection::open(&path).expect("read sqlite");
    let payload: String = connection
        .query_row(
            "SELECT payload FROM host_interaction_notification_outbox",
            [],
            |row| row.get(0),
        )
        .expect("notification payload");
    assert!(payload.contains("Choose normally"));
    for secret in [
        "secret-api",
        "hunter2",
        "bearer-secret",
        "sk-live-secret",
        "token-secret",
    ] {
        assert!(!payload.contains(secret), "secret leaked: {secret}");
    }
    assert!(payload.matches("[credential redacted]").count() >= 5);
    assert!(payload.contains("[external locator redacted]"));
    assert!(!payload.contains("example.invalid"));
}

#[test]
fn controller_suspend_resume_is_fenced_and_replayable() {
    let store = InMemoryCheckpointStore::new();
    let checkpoint = minimal_checkpoint();
    let key = checkpoint.checkpoint_key.clone();
    let handle =
        ControllerHandle::new(&key, &checkpoint.root_run_id, &checkpoint.trace_id).expect("handle");
    store.create_checkpoint(checkpoint).expect("create");
    let suspend = ControllerCommand::new(
        "command-suspend",
        handle.clone(),
        1,
        0,
        ControllerCommandVariant::Suspend,
    )
    .expect("suspend");
    let suspended = store
        .resolve_controller_command(suspend.clone())
        .expect("suspend command");
    assert_eq!(suspended.kind(), "applied");
    assert_eq!(
        store
            .load_checkpoint(&key)
            .expect("load")
            .expect("checkpoint")
            .status,
        vv_agent::CheckpointStatus::Suspended
    );
    assert_eq!(
        store
            .resolve_controller_command(suspend)
            .expect("suspend replay")
            .kind(),
        "replayed"
    );
    let resume = ControllerCommand::new(
        "command-resume",
        handle,
        1,
        1,
        ControllerCommandVariant::Resume,
    )
    .expect("resume");
    let resumed = store
        .resolve_controller_command(resume)
        .expect("resume command");
    match resumed {
        ControllerCommandResolution::Applied { wake, .. } => {
            assert_eq!(wake.action, "recovery_dispatch");
            assert_eq!(wake.logical_cycle, 1);
        }
        other => panic!("unexpected resolution: {other:?}"),
    }
    assert_eq!(
        store
            .load_checkpoint(&key)
            .expect("load")
            .expect("checkpoint")
            .status,
        vv_agent::CheckpointStatus::Running
    );
}

#[test]
fn controller_cancel_is_terminal_and_never_fabricates_a_second_transition() {
    let store = InMemoryCheckpointStore::new();
    let checkpoint = minimal_checkpoint();
    let key = checkpoint.checkpoint_key.clone();
    let handle =
        ControllerHandle::new(&key, &checkpoint.root_run_id, &checkpoint.trace_id).expect("handle");
    store.create_checkpoint(checkpoint).expect("create");
    let cancel = ControllerCommand::new(
        "command-cancel",
        handle.clone(),
        1,
        0,
        ControllerCommandVariant::Cancel,
    )
    .expect("cancel");
    store
        .resolve_controller_command(cancel)
        .expect("cancel command");
    let terminal = store
        .load_checkpoint(&key)
        .expect("load")
        .expect("checkpoint");
    assert_eq!(terminal.status, vv_agent::CheckpointStatus::Failed);
    assert!(terminal.terminal_result.is_some());
    let stale = ControllerCommand::new(
        "command-cancel-stale",
        handle,
        1,
        0,
        ControllerCommandVariant::Resume,
    )
    .expect("stale command");
    assert!(matches!(
        store
            .resolve_controller_command(stale)
            .expect("terminal command is a closed rejected resolution"),
        ControllerCommandResolution::Rejected { error }
            if error.starts_with("controller_command_stale:")
    ));
    assert_eq!(
        store
            .load_checkpoint(&key)
            .expect("reload")
            .expect("checkpoint")
            .revision,
        1
    );
}
