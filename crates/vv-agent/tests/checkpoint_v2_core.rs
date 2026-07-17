use std::collections::BTreeMap;

use base64::Engine as _;
use serde_json::{json, Value};
use tempfile::tempdir;
use vv_agent::runtime::checkpoint_codec_v2::{checkpoint_v2_from_value, checkpoint_v2_to_value};
use vv_agent::runtime::state_v2::validate_extension_state_size;
use vv_agent::{
    canonical_json_bytes, decode_checkpoint, encode_checkpoint_v1, event_payload_digest,
    migrate_terminal_v1, model_request_digest, operation_request_digest, run_definition_digest,
    tool_request_digest, CapabilityRef, CheckpointStatus, CheckpointStoreV2, CheckpointV2,
    ClaimMode, DecodedCheckpoint, EventCursor, EventOutboxEntry, ExtensionStateEntry,
    InMemoryCheckpointStoreV2, Message, OperationJournalEntry, OperationKind, OperationState,
    RedisCheckpointStoreV2, SqliteCheckpointStoreV2,
};

const CODEC_FIXTURE: &str = include_str!("fixtures/parity/checkpoint_codec_v2.json");
const V1_FIXTURE: &str = include_str!("fixtures/parity/checkpoint_codec_v1.json");
const DEFINITION_FIXTURE: &str = include_str!("fixtures/parity/run_definition_v1.json");
const JOURNAL_FIXTURE: &str = include_str!("fixtures/parity/operation_journal_v1.json");
const STORE_FIXTURE: &str = include_str!("fixtures/parity/checkpoint_store_v2.json");

fn fixture(raw: &str) -> Value {
    serde_json::from_str(raw).expect("valid parity fixture")
}

fn codec_case(name: &str) -> Value {
    fixture(CODEC_FIXTURE)["valid_cases"]
        .as_array()
        .expect("valid cases")
        .iter()
        .find(|case| case["name"] == name)
        .unwrap_or_else(|| panic!("missing codec case {name}"))["payload"]
        .clone()
}

fn journal_case(name: &str) -> OperationJournalEntry {
    let value = fixture(JOURNAL_FIXTURE)["valid_entries"]
        .as_array()
        .expect("valid journal entries")
        .iter()
        .find(|case| case["name"] == name)
        .unwrap_or_else(|| panic!("missing journal case {name}"))["entry"]
        .clone();
    OperationJournalEntry::from_value(&value).expect("valid journal entry")
}

fn minimal_checkpoint(key: &str) -> CheckpointV2 {
    let mut payload = codec_case("minimal_running");
    payload["checkpoint_key"] = Value::String(key.to_string());
    checkpoint_v2_from_value(&payload, 262_144).unwrap()
}

fn delivery_cursor(event_id: &str, sequence: u64) -> EventCursor {
    EventCursor::new(
        CapabilityRef::new("events.tenant", "1").unwrap(),
        json!({"sequence": sequence}),
        Some(event_id.to_string()),
    )
}

#[test]
fn rfc8785_definition_operation_and_event_vectors_match() {
    let definition_fixture = fixture(DEFINITION_FIXTURE);
    for case in definition_fixture["golden_cases"].as_array().unwrap() {
        let definition = &case["definition"];
        let expected = base64::engine::general_purpose::STANDARD
            .decode(case["canonical_json_base64"].as_str().unwrap())
            .unwrap();
        assert_eq!(
            canonical_json_bytes(definition, "run definition").unwrap(),
            expected
        );
        assert_eq!(run_definition_digest(definition).unwrap(), case["sha256"]);
    }

    let journal_fixture = fixture(JOURNAL_FIXTURE);
    for case in journal_fixture["request_digest"]["golden_cases"]
        .as_array()
        .unwrap()
    {
        let request = &case["request"];
        let kind = match request["kind"].as_str().unwrap() {
            "model" => OperationKind::Model,
            "tool" => OperationKind::Tool,
            other => panic!("unexpected operation kind {other}"),
        };
        assert_eq!(
            operation_request_digest(kind, request).unwrap(),
            case["sha256"]
        );
    }
    let planned = journal_case("model_planned");
    planned
        .verify_request(&journal_fixture["request_digest"]["golden_cases"][0]["request"])
        .unwrap();
    let mut changed = journal_fixture["request_digest"]["golden_cases"][0]["request"].clone();
    changed["request"]["messages"][0]["content"] = json!("different");
    assert_eq!(
        planned.verify_request(&changed).unwrap_err().code(),
        "checkpoint_journal_integrity_mismatch"
    );
    let model = &journal_fixture["request_digest"]["golden_cases"][0];
    assert_eq!(
        model_request_digest(&model["request"]).unwrap(),
        model["sha256"]
    );
    let tool = &journal_fixture["request_digest"]["golden_cases"][1];
    let payload = &tool["request"]["request"];
    assert_eq!(
        tool_request_digest(
            payload["tool_call_id"].as_str().unwrap(),
            payload["tool_name"].as_str().unwrap(),
            &payload["arguments"],
            payload["idempotency_key"].as_str().unwrap(),
        )
        .unwrap(),
        tool["sha256"]
    );

    let event = &fixture(STORE_FIXTURE)["event_payload_digest"]["golden_cases"][0];
    assert_eq!(
        event_payload_digest(&event["event"]).unwrap(),
        event["sha256"]
    );
}

#[test]
fn codec_round_trips_canonical_unknown_fields_and_strict_discriminator() {
    let expected = fixture(CODEC_FIXTURE)["canonical_checkpoint"].clone();
    let checkpoint = checkpoint_v2_from_value(&expected, 262_144).unwrap();
    assert_eq!(
        checkpoint.unknown_fields["vendor_future"],
        json!({"preserve": true})
    );
    assert_eq!(
        checkpoint_v2_to_value(&checkpoint, 262_144).unwrap(),
        expected
    );

    let unknown_schema = json!({"schema_version": "vv-agent.checkpoint.v3"});
    let error = decode_checkpoint(&unknown_schema.to_string()).unwrap_err();
    assert_eq!(error.code(), "checkpoint_schema_unsupported");

    let mut missing_definition_schema = codec_case("minimal_running");
    missing_definition_schema
        .as_object_mut()
        .unwrap()
        .remove("run_definition_schema");
    let error = checkpoint_v2_from_value(&missing_definition_schema, 262_144).unwrap_err();
    assert_eq!(error.code(), "checkpoint_definition_schema_unsupported");

    let duplicate = decode_checkpoint(r#"{"task_id":"a","task_id":"b"}"#).unwrap_err();
    assert_eq!(duplicate.code(), "checkpoint_json_invalid");

    let mut missing_required = codec_case("minimal_running");
    missing_required
        .as_object_mut()
        .unwrap()
        .remove("terminal_acknowledged");
    assert_eq!(
        checkpoint_v2_from_value(&missing_required, 262_144)
            .unwrap_err()
            .code(),
        "checkpoint_field_invalid"
    );

    let invalid_fixture = fixture(CODEC_FIXTURE);
    let bad_digest = invalid_fixture["invalid_cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "bad_definition_digest")
        .unwrap();
    assert_eq!(
        checkpoint_v2_from_value(&bad_digest["payload"], 262_144)
            .unwrap_err()
            .code(),
        "checkpoint_definition_digest_invalid"
    );
}

#[test]
fn v1_encoder_is_unchanged_and_terminal_migration_is_explicit() {
    let v1 = fixture(V1_FIXTURE)["canonical"].clone();
    let v1_json = serde_json::to_string(&v1).unwrap();
    let DecodedCheckpoint::V1(checkpoint) = decode_checkpoint(&v1_json).unwrap() else {
        panic!("absent discriminator must decode as v1");
    };
    assert_eq!(encode_checkpoint_v1(&checkpoint).unwrap(), v1_json);

    let migration_fixture = fixture(CODEC_FIXTURE);
    let terminal_source = migration_fixture["migration_cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "terminal_v1_explicit_migration")
        .unwrap()["source"]
        .clone();
    let DecodedCheckpoint::V1(terminal) =
        decode_checkpoint(&serde_json::to_string(&terminal_source).unwrap()).unwrap()
    else {
        panic!("migration source must decode as v1");
    };
    let definition = fixture(DEFINITION_FIXTURE)["golden_cases"][0]["definition"].clone();
    let migrated = migrate_terminal_v1(
        &terminal,
        "migrated-terminal",
        "run-migrated",
        "trace-migrated",
        definition,
    )
    .unwrap();
    assert_eq!(migrated.checkpoint_key, "migrated-terminal");
    assert_eq!(migrated.status, CheckpointStatus::Completed);
    assert!(migrated.terminal_result.is_some());
}

#[test]
fn extension_jcs_limits_count_complete_entries() {
    let mut extensions = BTreeMap::new();
    extensions.insert(
        "com.example.limit".to_string(),
        ExtensionStateEntry {
            version: "1".to_string(),
            required: false,
            state: Value::String("x".repeat(65_493)),
        },
    );
    validate_extension_state_size(&extensions, 65_536).unwrap();
    extensions.get_mut("com.example.limit").unwrap().state = Value::String("x".repeat(65_494));
    let error = validate_extension_state_size(&extensions, u64::MAX).unwrap_err();
    assert_eq!(error.code(), "checkpoint_extension_entry_too_large");
}

#[test]
fn journal_invalid_cases_return_fixture_codes() {
    let fixture = fixture(JOURNAL_FIXTURE);
    for case in fixture["valid_entries"].as_array().unwrap() {
        OperationJournalEntry::from_value(&case["entry"]).unwrap();
    }
    for case in fixture["invalid_entries"].as_array().unwrap() {
        let error = OperationJournalEntry::from_value(&case["entry"]).unwrap_err();
        assert_eq!(
            error.code(),
            case["error_code"].as_str().unwrap(),
            "{}",
            case["name"]
        );
    }
}

fn exercise_store(store: &dyn CheckpointStoreV2, key: &str) {
    let mut payload = codec_case("minimal_running");
    payload["checkpoint_key"] = Value::String(key.to_string());
    let checkpoint = checkpoint_v2_from_value(&payload, 262_144).unwrap();
    assert!(store.create_checkpoint_v2(checkpoint).unwrap());

    let continued = store
        .claim_checkpoint_v2(key, 1, "owner-a", 200, 100, ClaimMode::Continue)
        .unwrap()
        .unwrap();
    assert_eq!(continued.resume_attempt, 1);
    assert_eq!(continued.revision, 1);
    assert!(store
        .claim_checkpoint_v2(key, 1, "owner-b", 300, 199, ClaimMode::Recovery)
        .unwrap()
        .is_none());

    let recovered = store
        .claim_checkpoint_v2(key, 1, "owner-b", 300, 200, ClaimMode::Recovery)
        .unwrap()
        .unwrap();
    assert_eq!(recovered.resume_attempt, 2);
    assert_eq!(recovered.revision, 2);

    let mut progress = recovered;
    progress.model_call_journal = vec![journal_case("model_started")];
    assert!(store
        .progress_checkpoint_v2(progress, "owner-b", 2)
        .unwrap());
    assert!(store
        .renew_checkpoint_claim_v2(key, "owner-b", 400, 250)
        .unwrap());
    let mut ambiguous = store.load_checkpoint_v2(key).unwrap().unwrap();
    ambiguous.model_call_journal[0].mark_ambiguous().unwrap();
    assert!(store
        .suspend_checkpoint_v2(ambiguous, "owner-b", 3)
        .unwrap());
    let suspended = store.load_checkpoint_v2(key).unwrap().unwrap();
    assert_eq!(suspended.status, CheckpointStatus::ReconciliationRequired);
    assert!(suspended.claim_token.is_none());
    assert_eq!(suspended.resume_attempt, 2);

    let mut resolving = store
        .claim_checkpoint_v2(key, 1, "resolver", 600, 500, ClaimMode::Recovery)
        .unwrap()
        .unwrap();
    assert_eq!(resolving.resume_attempt, 3);
    resolving.model_call_journal.clear();
    resolving.cycle_index = 1;
    let revision = resolving.revision;
    assert!(store
        .commit_checkpoint_v2(resolving, "resolver", revision)
        .unwrap());

    let mut terminal = store.load_checkpoint_v2(key).unwrap().unwrap();
    terminal.status = CheckpointStatus::Completed;
    terminal.terminal_result = Some(json!({"status": "completed", "final_answer": "done"}));
    let revision = terminal.revision;
    assert!(store.finalize_checkpoint_v2(terminal, revision).unwrap());
    let terminal = store.load_checkpoint_v2(key).unwrap().unwrap();
    assert!(store
        .acknowledge_terminal_v2(key, terminal.revision)
        .unwrap());
    let retained = store.load_checkpoint_v2(key).unwrap().unwrap();
    assert!(retained.terminal_acknowledged);
    assert!(retained.terminal_result.is_some());
    assert!(!store
        .acknowledge_terminal_v2(key, retained.revision)
        .unwrap());
}

#[test]
fn in_memory_store_has_atomic_continue_recovery_suspend_finalize_and_ack() {
    exercise_store(&InMemoryCheckpointStoreV2::new(), "memory-core");
}

#[test]
fn sqlite_store_has_atomic_continue_recovery_suspend_finalize_and_ack() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("checkpoint-v2.sqlite3");
    let store = SqliteCheckpointStoreV2::new(&path).unwrap();
    exercise_store(&store, "sqlite-core");

    let connection = rusqlite::Connection::open(path).unwrap();
    let table_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'checkpoints_v2'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(table_count, 1);
}

#[test]
fn cross_runtime_sqlite_v2_probe_from_environment() {
    let Ok(path) = std::env::var("VV_AGENT_CROSS_RUNTIME_V2_DB") else {
        return;
    };
    let mode = std::env::var("VV_AGENT_CROSS_RUNTIME_V2_MODE")
        .unwrap_or_else(|_| "read_python".to_string());
    let store = SqliteCheckpointStoreV2::new(path).expect("cross-runtime SQLite v2 store");

    match mode.as_str() {
        "read_python" => {
            let checkpoint = store
                .load_checkpoint_v2("python-wrote-v2")
                .expect("load Python checkpoint v2")
                .expect("Python checkpoint v2 exists");
            assert_eq!(checkpoint.messages, vec![Message::user("from Python v2")]);
            assert_eq!(
                checkpoint.shared_state,
                BTreeMap::from([
                    ("format".to_string(), json!("checkpoint-v2")),
                    ("writer".to_string(), json!("python")),
                ])
            );
            assert_eq!(
                checkpoint.run_definition_digest,
                run_definition_digest(&checkpoint.run_definition).unwrap()
            );
        }
        "write_rust" => {
            let mut checkpoint = minimal_checkpoint("rust-wrote-v2");
            checkpoint.messages = vec![Message::user("from Rust v2")];
            checkpoint.shared_state = BTreeMap::from([
                ("format".to_string(), json!("checkpoint-v2")),
                ("writer".to_string(), json!("rust")),
            ]);
            assert!(store.create_checkpoint_v2(checkpoint).unwrap());
        }
        other => panic!("unknown cross-runtime v2 mode: {other}"),
    }
}

fn exercise_store_052(store: &dyn CheckpointStoreV2, prefix: &str) {
    let failure_key = format!("{prefix}-claimed-failure");
    assert!(store
        .create_checkpoint_v2(minimal_checkpoint(&failure_key))
        .unwrap());
    let mut failure = store
        .claim_checkpoint_v2(
            &failure_key,
            1,
            "failure-owner",
            200,
            100,
            ClaimMode::Continue,
        )
        .unwrap()
        .unwrap();
    failure.model_call_journal = vec![journal_case("model_failed")];
    failure.status = CheckpointStatus::Failed;
    failure.terminal_result = Some(json!({"status": "failed", "error": "provider_rejected"}));
    let failure_revision = failure.revision;
    assert!(!store
        .finalize_claimed_v2(failure.clone(), "failure-owner", failure_revision + 1,)
        .unwrap());
    assert!(!store
        .finalize_claimed_v2(failure.clone(), "wrong-owner", failure_revision)
        .unwrap());
    let unchanged = store.load_checkpoint_v2(&failure_key).unwrap().unwrap();
    assert_eq!(unchanged.revision, failure_revision);
    assert_eq!(unchanged.claim_token.as_deref(), Some("failure-owner"));
    assert!(store
        .finalize_claimed_v2(failure, "failure-owner", failure_revision)
        .unwrap());
    let finalized = store.load_checkpoint_v2(&failure_key).unwrap().unwrap();
    assert_eq!(finalized.revision, failure_revision + 1);
    assert_eq!(finalized.status, CheckpointStatus::Failed);
    assert!(finalized.claim_token.is_none());
    assert!(finalized.claimed_cycle.is_none());
    assert!(finalized.lease_expires_at_ms.is_none());
    assert!(finalized.model_call_journal.is_empty());
    assert!(finalized.terminal_result.is_some());

    let abort_key = format!("{prefix}-claimed-abort");
    let mut abort = checkpoint_v2_from_value(
        &codec_case("reconciliation_required_retains_ambiguous_journal"),
        262_144,
    )
    .unwrap();
    abort.checkpoint_key = abort_key.clone();
    abort.revision = 0;
    abort.resume_attempt = 1;
    assert!(store.create_checkpoint_v2(abort).unwrap());
    let mut abort = store
        .claim_checkpoint_v2(&abort_key, 2, "abort-owner", 400, 300, ClaimMode::Recovery)
        .unwrap()
        .unwrap();
    abort.status = CheckpointStatus::Failed;
    abort.terminal_result = Some(json!({
        "status": "failed",
        "error_code": "operator_abort_with_unknown_outcome",
        "resume_observation": {
            "operation_id": abort.tool_journal[0].operation_id,
            "operation_kind": "tool",
            "cycle_index": 2,
            "state": "ambiguous",
            "risk": "unknown external tool outcome",
            "idempotency_support": "unknown"
        }
    }));
    let abort_revision = abort.revision;
    assert!(store
        .finalize_claimed_v2(abort, "abort-owner", abort_revision)
        .unwrap());
    let abort = store.load_checkpoint_v2(&abort_key).unwrap().unwrap();
    assert_eq!(abort.revision, abort_revision + 1);
    assert!(abort.claim_token.is_none());
    assert_eq!(abort.tool_journal.len(), 1);
    assert_eq!(abort.tool_journal[0].state, OperationState::Ambiguous);
    assert!(abort.terminal_result.as_ref().unwrap()["resume_observation"].is_object());

    let running_event_key = format!("{prefix}-running-event");
    let event = json!({"type": "checkpoint_created"});
    let pending = EventOutboxEntry::pending("evt-running", event).unwrap();
    let digest = pending.payload_digest.clone();
    let mut running = minimal_checkpoint(&running_event_key);
    running.event_outbox.push(pending);
    assert!(store.create_checkpoint_v2(running).unwrap());
    let running = store
        .claim_checkpoint_v2(
            &running_event_key,
            1,
            "event-owner",
            700,
            600,
            ClaimMode::Continue,
        )
        .unwrap()
        .unwrap();
    let cursor = delivery_cursor("evt-running", 1);
    assert!(!store
        .record_event_delivery_v2(
            &running_event_key,
            Some("event-owner"),
            running.revision + 1,
            "evt-running",
            &digest,
            cursor.clone(),
        )
        .unwrap());
    assert!(!store
        .record_event_delivery_v2(
            &running_event_key,
            Some("wrong-owner"),
            running.revision,
            "evt-running",
            &digest,
            cursor.clone(),
        )
        .unwrap());
    assert!(!store
        .record_event_delivery_v2(
            &running_event_key,
            Some("event-owner"),
            running.revision,
            "evt-running",
            &"b".repeat(64),
            cursor.clone(),
        )
        .unwrap());
    assert!(store
        .record_event_delivery_v2(
            &running_event_key,
            Some("event-owner"),
            running.revision,
            "evt-running",
            &digest,
            cursor.clone(),
        )
        .unwrap());
    let delivered = store
        .load_checkpoint_v2(&running_event_key)
        .unwrap()
        .unwrap();
    assert_eq!(delivered.revision, running.revision + 1);
    assert_eq!(delivered.claim_token.as_deref(), Some("event-owner"));
    assert_eq!(delivered.lease_expires_at_ms, Some(700));
    assert_eq!(delivered.event_outbox[0].state, "delivered");
    assert_eq!(
        delivered.event_outbox[0].cursor,
        Some(serde_json::to_value(&cursor).unwrap())
    );
    assert_eq!(delivered.event_cursor, Some(cursor));

    let terminal_event_key = format!("{prefix}-terminal-event");
    let pending =
        EventOutboxEntry::pending("evt-terminal", json!({"type": "checkpoint_created"})).unwrap();
    let digest = pending.payload_digest.clone();
    let mut terminal = minimal_checkpoint(&terminal_event_key);
    terminal.status = CheckpointStatus::Completed;
    terminal.terminal_result = Some(json!({"status": "completed", "final_answer": "done"}));
    terminal.event_outbox.push(pending);
    let terminal_receipt = terminal.terminal_result.clone();
    assert!(store.create_checkpoint_v2(terminal).unwrap());
    let cursor = delivery_cursor("evt-terminal", 2);
    assert!(!store
        .record_event_delivery_v2(
            &terminal_event_key,
            None,
            1,
            "evt-terminal",
            &digest,
            cursor.clone(),
        )
        .unwrap());
    assert!(!store
        .record_event_delivery_v2(
            &terminal_event_key,
            Some("unexpected-owner"),
            0,
            "evt-terminal",
            &digest,
            cursor.clone(),
        )
        .unwrap());
    assert!(store
        .record_event_delivery_v2(
            &terminal_event_key,
            None,
            0,
            "evt-terminal",
            &digest,
            cursor,
        )
        .unwrap());
    let terminal = store
        .load_checkpoint_v2(&terminal_event_key)
        .unwrap()
        .unwrap();
    assert_eq!(terminal.revision, 1);
    assert_eq!(terminal.status, CheckpointStatus::Completed);
    assert_eq!(terminal.terminal_result, terminal_receipt);
    assert_eq!(terminal.event_outbox[0].state, "delivered");
    assert_eq!(
        terminal
            .event_cursor
            .as_ref()
            .unwrap()
            .last_event_id
            .as_deref(),
        Some("evt-terminal")
    );
}

#[test]
fn in_memory_store_adopts_052_claimed_finalize_and_event_delivery() {
    exercise_store_052(&InMemoryCheckpointStoreV2::new(), "memory-052");
}

#[test]
fn sqlite_store_adopts_052_claimed_finalize_and_event_delivery() {
    let directory = tempdir().unwrap();
    let store =
        SqliteCheckpointStoreV2::new(directory.path().join("checkpoint-052.sqlite3")).unwrap();
    exercise_store_052(&store, "sqlite-052");
}

#[test]
fn store_rejects_run_definition_replacement() {
    let store = InMemoryCheckpointStoreV2::new();
    let checkpoint = checkpoint_v2_from_value(&codec_case("minimal_running"), 262_144).unwrap();
    let key = checkpoint.checkpoint_key.clone();
    assert!(store.create_checkpoint_v2(checkpoint).unwrap());
    let mut claimed = store
        .claim_checkpoint_v2(&key, 1, "owner", 200, 100, ClaimMode::Continue)
        .unwrap()
        .unwrap();
    claimed.run_definition["root_input"] = json!("replacement");
    claimed.run_definition_digest = run_definition_digest(&claimed.run_definition).unwrap();
    let revision = claimed.revision;
    assert!(!store
        .progress_checkpoint_v2(claimed, "owner", revision)
        .unwrap());
}

#[test]
fn canonical_outbox_round_trips_and_delivery_verifies_digest() {
    let checkpoint =
        checkpoint_v2_from_value(&fixture(CODEC_FIXTURE)["canonical_checkpoint"], 262_144).unwrap();
    let placeholder = &checkpoint.event_outbox[0];
    placeholder.verify_payload().unwrap();

    let entry = EventOutboxEntry::pending("evt-1", json!({"type": "checkpoint_created"})).unwrap();
    entry.verify_payload().unwrap();
}

#[test]
fn redis_v2_keys_match_contract_vectors() {
    let fixture = fixture(STORE_FIXTURE);
    let operations = fixture["operations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|operation| operation["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(operations.contains(&"finalize_claimed_v2"));
    assert!(operations.contains(&"record_event_delivery_v2"));
    for vector in fixture["redis_key_vectors"].as_array().unwrap() {
        let key = vector["checkpoint_key"].as_str().unwrap();
        assert_eq!(
            RedisCheckpointStoreV2::checkpoint_v2_key(key),
            vector["v2_data_key"]
        );
        assert_eq!(
            RedisCheckpointStoreV2::checkpoint_v2_lease_key(key),
            vector["v2_lease_key"]
        );
    }
}

#[test]
#[ignore = "requires VV_AGENT_REDIS_URL and a live Redis instance"]
fn redis_store_adopts_052_claimed_finalize_and_event_delivery() {
    let redis_url = std::env::var("VV_AGENT_REDIS_URL").expect("VV_AGENT_REDIS_URL");
    let store = RedisCheckpointStoreV2::new(redis_url).unwrap();
    let prefix = format!("redis-052-{}", uuid::Uuid::new_v4());
    exercise_store_052(&store, &prefix);
}
