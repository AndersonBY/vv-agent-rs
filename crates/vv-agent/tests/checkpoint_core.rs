use std::collections::BTreeMap;

use base64::Engine as _;
use serde_json::{json, Value};
use tempfile::tempdir;
use vv_agent::runtime::checkpoint_codec::{checkpoint_from_value, checkpoint_to_value};
use vv_agent::runtime::state::validate_extension_state_size;
use vv_agent::{
    canonical_json_bytes, checkpoint_from_json, event_payload_digest, model_request_digest,
    operation_request_digest, run_definition_digest, tool_request_digest, AgentResult, AgentStatus,
    CapabilityRef, Checkpoint, CheckpointStatus, CheckpointStore, ClaimMode, CompletionReason,
    EventCursor, EventOutboxEntry, ExtensionStateEntry, InMemoryCheckpointStore, Message,
    OperationJournalEntry, OperationKind, OperationState, RedisCheckpointStore, ResumeObservation,
    RunEvent, SqliteCheckpointStore, ToolIdempotency,
};

const CODEC_FIXTURE: &str = include_str!("fixtures/parity/checkpoint_codec.json");
const DEFINITION_FIXTURE: &str = include_str!("fixtures/parity/run_definition.json");
const JOURNAL_FIXTURE: &str = include_str!("fixtures/parity/operation_journal.json");
const STORE_FIXTURE: &str = include_str!("fixtures/parity/checkpoint_store.json");

fn fixture(raw: &str) -> Value {
    serde_json::from_str(raw).expect("valid parity fixture")
}

fn current_event(event_id: &str) -> Value {
    serde_json::to_value(
        RunEvent::run_started(
            format!("run-{event_id}"),
            format!("trace-{event_id}"),
            "assistant",
            "resume the current run",
        )
        .with_event_id(event_id)
        .unwrap(),
    )
    .unwrap()
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

fn minimal_checkpoint(key: &str) -> Checkpoint {
    let mut payload = codec_case("minimal_running");
    payload["checkpoint_key"] = Value::String(key.to_string());
    checkpoint_from_value(&payload, 262_144).unwrap()
}

fn terminal_result(checkpoint: &Checkpoint, status: AgentStatus) -> AgentResult {
    AgentResult {
        status,
        messages: checkpoint.messages.clone(),
        cycles: checkpoint.cycles.clone(),
        budget_usage: checkpoint.budget_usage.clone(),
        checkpoint_key: Some(checkpoint.checkpoint_key.clone()),
        shared_state: checkpoint.shared_state.clone(),
        token_usage: vv_agent::runtime::summarize_task_token_usage(&checkpoint.model_calls),
        ..AgentResult::default()
    }
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
fn codec_round_trips_canonical_payload_and_rejects_invalid_input() {
    let expected = fixture(CODEC_FIXTURE)["canonical_checkpoint"].clone();
    let checkpoint = checkpoint_from_value(&expected, 262_144).unwrap();
    assert_eq!(checkpoint_to_value(&checkpoint, 262_144).unwrap(), expected);

    let unknown_schema = json!({"schema_version": "vv-agent.checkpoint.v4"});
    let error = checkpoint_from_value(&unknown_schema, 262_144).unwrap_err();
    assert_eq!(error.code(), "checkpoint_schema_unsupported");

    let mut missing_definition_schema = codec_case("minimal_running");
    missing_definition_schema
        .as_object_mut()
        .unwrap()
        .remove("run_definition_schema");
    let error = checkpoint_from_value(&missing_definition_schema, 262_144).unwrap_err();
    assert_eq!(error.code(), "checkpoint_definition_schema_unsupported");

    let duplicate = checkpoint_from_json(r#"{"task_id":"a","task_id":"b"}"#, 262_144).unwrap_err();
    assert_eq!(duplicate.code(), "checkpoint_json_invalid");

    let mut missing_required = codec_case("minimal_running");
    missing_required
        .as_object_mut()
        .unwrap()
        .remove("terminal_acknowledged");
    assert_eq!(
        checkpoint_from_value(&missing_required, 262_144)
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
        checkpoint_from_value(&bad_digest["payload"], 262_144)
            .unwrap_err()
            .code(),
        "checkpoint_definition_digest_invalid"
    );
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
        let entry = if let Some(base_name) = case.get("base_valid_entry").and_then(Value::as_str) {
            let mut entry = journal_case(base_name).to_value();
            let mutation = &case["mutation"];
            if let Some(field) = mutation.get("remove").and_then(Value::as_str) {
                entry.as_object_mut().unwrap().remove(field);
            }
            if let Some(replacements) = mutation.get("replace").and_then(Value::as_object) {
                entry.as_object_mut().unwrap().extend(replacements.clone());
            }
            entry
        } else {
            case["entry"].clone()
        };
        let error = OperationJournalEntry::from_value(&entry).unwrap_err();
        assert_eq!(
            error.code(),
            case["error_code"].as_str().unwrap(),
            "{}",
            case["name"]
        );
    }
}

fn exercise_store(store: &dyn CheckpointStore, key: &str) {
    let mut payload = codec_case("minimal_running");
    payload["checkpoint_key"] = Value::String(key.to_string());
    let checkpoint = checkpoint_from_value(&payload, 262_144).unwrap();
    assert!(store.create_checkpoint(checkpoint).unwrap());

    let continued = store
        .claim_checkpoint(key, 1, "owner-a", 200, 100, ClaimMode::Continue)
        .unwrap()
        .unwrap();
    assert_eq!(continued.resume_attempt, 1);
    assert_eq!(continued.revision, 1);
    assert!(store
        .claim_checkpoint(key, 1, "owner-b", 300, 199, ClaimMode::Recovery)
        .unwrap()
        .is_none());

    let recovered = store
        .claim_checkpoint(key, 1, "owner-b", 300, 200, ClaimMode::Recovery)
        .unwrap()
        .unwrap();
    assert_eq!(recovered.resume_attempt, 2);
    assert_eq!(recovered.revision, 2);

    let mut progress = recovered;
    progress.tool_journal = vec![journal_case("tool_started")];
    assert!(store.progress_checkpoint(progress, "owner-b", 2).unwrap());
    assert!(store
        .renew_checkpoint_claim(key, "owner-b", 400, 250)
        .unwrap());
    let mut ambiguous = store.load_checkpoint(key).unwrap().unwrap();
    ambiguous.tool_journal[0].mark_ambiguous().unwrap();
    assert!(store.suspend_checkpoint(ambiguous, "owner-b", 3).unwrap());
    let suspended = store.load_checkpoint(key).unwrap().unwrap();
    assert_eq!(suspended.status, CheckpointStatus::ReconciliationRequired);
    assert!(suspended.claim_token.is_none());
    assert_eq!(suspended.resume_attempt, 2);

    let mut resolving = store
        .claim_checkpoint(key, 1, "resolver", 600, 500, ClaimMode::Recovery)
        .unwrap()
        .unwrap();
    assert_eq!(resolving.resume_attempt, 3);
    resolving.tool_journal.clear();
    resolving.cycle_index = 1;
    let revision = resolving.revision;
    assert!(store
        .commit_checkpoint(resolving, "resolver", revision)
        .unwrap());

    let mut terminal = store.load_checkpoint(key).unwrap().unwrap();
    terminal.status = CheckpointStatus::Completed;
    let mut result = terminal_result(&terminal, AgentStatus::Completed);
    result.completion_reason = Some(CompletionReason::NoToolFinish);
    result.final_answer = Some("done".to_string());
    terminal.terminal_result = Some(result.to_dict());
    let revision = terminal.revision;
    assert!(store.finalize_checkpoint(terminal, revision).unwrap());
    let terminal = store.load_checkpoint(key).unwrap().unwrap();
    assert!(store.acknowledge_terminal(key, terminal.revision).unwrap());
    let retained = store.load_checkpoint(key).unwrap().unwrap();
    assert!(retained.terminal_acknowledged);
    assert!(retained.terminal_result.is_some());
    assert!(!store.acknowledge_terminal(key, retained.revision).unwrap());
}

#[test]
fn in_memory_store_has_atomic_continue_recovery_suspend_finalize_and_ack() {
    exercise_store(&InMemoryCheckpointStore::new(), "memory-core");
}

#[test]
fn sqlite_store_has_atomic_continue_recovery_suspend_finalize_and_ack() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("checkpoint-v3.sqlite3");
    let store = SqliteCheckpointStore::new(&path).unwrap();
    exercise_store(&store, "sqlite-core");

    let connection = rusqlite::Connection::open(path).unwrap();
    let table_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'checkpoints'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(table_count, 1);
}

#[test]
fn sqlite_store_rejects_non_current_checkpoints_table_schema() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("invalid-checkpoint.sqlite3");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch("CREATE TABLE checkpoints (task_id TEXT PRIMARY KEY);")
        .unwrap();
    drop(connection);

    let error = SqliteCheckpointStore::new(path).expect_err("invalid schema must be rejected");
    assert_eq!(error.code(), "checkpoint_store_schema_mismatch");
}

#[test]
fn sqlite_store_rejects_missing_current_index() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("missing-index.sqlite3");
    let store = SqliteCheckpointStore::new(&path).unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch("DROP INDEX checkpoints_status_idx;")
        .unwrap();
    drop(connection);

    let error = SqliteCheckpointStore::new(path).expect_err("missing index must be rejected");
    assert_eq!(error.code(), "checkpoint_store_schema_mismatch");
}

#[test]
fn sqlite_store_ignores_unrelated_checkpoint_prefixed_tables() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("unrelated-checkpoint.sqlite3");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch("CREATE TABLE checkpoint_archive (task_id TEXT PRIMARY KEY);")
        .unwrap();
    drop(connection);

    let store = SqliteCheckpointStore::new(&path).expect("unrelated table is not a schema signal");
    drop(store);
    let connection = rusqlite::Connection::open(path).unwrap();
    let table_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('checkpoints', 'checkpoint_archive')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(table_count, 2);
}

#[test]
fn cross_runtime_sqlite_probe_from_environment() {
    let Ok(path) = std::env::var("VV_AGENT_CROSS_RUNTIME_DB") else {
        return;
    };
    let mode =
        std::env::var("VV_AGENT_CROSS_RUNTIME_MODE").unwrap_or_else(|_| "read_python".to_string());
    let store = SqliteCheckpointStore::new(path).expect("cross-runtime SQLite store");

    match mode.as_str() {
        "read_python" => {
            let checkpoint = store
                .load_checkpoint("python-wrote")
                .expect("load Python checkpoint")
                .expect("Python checkpoint exists");
            assert_eq!(checkpoint.messages, vec![Message::user("from Python")]);
            assert_eq!(
                checkpoint.shared_state,
                BTreeMap::from([
                    ("format".to_string(), json!("checkpoint")),
                    ("writer".to_string(), json!("python")),
                ])
            );
            assert_eq!(
                checkpoint.run_definition_digest,
                run_definition_digest(&checkpoint.run_definition).unwrap()
            );
        }
        "write_rust" => {
            let mut checkpoint = minimal_checkpoint("rust-wrote");
            checkpoint.messages = vec![Message::user("from Rust")];
            checkpoint.shared_state = BTreeMap::from([
                ("format".to_string(), json!("checkpoint")),
                ("writer".to_string(), json!("rust")),
            ]);
            assert!(store.create_checkpoint(checkpoint).unwrap());
        }
        other => panic!("unknown cross-runtime mode: {other}"),
    }
}

fn exercise_current_store_contract(store: &dyn CheckpointStore, prefix: &str) {
    let failure_key = format!("{prefix}-claimed-failure");
    assert!(store
        .create_checkpoint(minimal_checkpoint(&failure_key))
        .unwrap());
    let mut failure = store
        .claim_checkpoint(
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
    let mut failure_result = terminal_result(&failure, AgentStatus::Failed);
    failure_result.completion_reason = Some(CompletionReason::Failed);
    failure_result.error = Some("provider_rejected".to_string());
    failure_result.error_code = Some("provider_rejected".to_string());
    failure.terminal_result = Some(failure_result.to_dict());
    let failure_revision = failure.revision;
    assert!(!store
        .finalize_claimed_checkpoint(failure.clone(), "failure-owner", failure_revision + 1,)
        .unwrap());
    assert!(!store
        .finalize_claimed_checkpoint(failure.clone(), "wrong-owner", failure_revision)
        .unwrap());
    let unchanged = store.load_checkpoint(&failure_key).unwrap().unwrap();
    assert_eq!(unchanged.revision, failure_revision);
    assert_eq!(unchanged.claim_token.as_deref(), Some("failure-owner"));
    assert!(store
        .finalize_claimed_checkpoint(failure, "failure-owner", failure_revision)
        .unwrap());
    let finalized = store.load_checkpoint(&failure_key).unwrap().unwrap();
    assert_eq!(finalized.revision, failure_revision + 1);
    assert_eq!(finalized.status, CheckpointStatus::Failed);
    assert!(finalized.claim_token.is_none());
    assert!(finalized.claimed_cycle.is_none());
    assert!(finalized.lease_expires_at_ms.is_none());
    assert!(finalized.model_call_journal.is_empty());
    assert!(finalized.terminal_result.is_some());

    let abort_key = format!("{prefix}-claimed-abort");
    let mut abort = checkpoint_from_value(
        &codec_case("reconciliation_required_retains_ambiguous_journal"),
        262_144,
    )
    .unwrap();
    abort.checkpoint_key = abort_key.clone();
    abort.revision = 0;
    abort.resume_attempt = 1;
    assert!(store.create_checkpoint(abort).unwrap());
    let mut abort = store
        .claim_checkpoint(&abort_key, 2, "abort-owner", 400, 300, ClaimMode::Recovery)
        .unwrap()
        .unwrap();
    abort.status = CheckpointStatus::Failed;
    let mut abort_result = terminal_result(&abort, AgentStatus::Failed);
    abort_result.completion_reason = Some(CompletionReason::Failed);
    abort_result.error = Some("operator aborted with unknown external outcome".to_string());
    abort_result.error_code = Some("operator_abort_with_unknown_outcome".to_string());
    abort_result.resume_observation = Some(ResumeObservation {
        operation_id: abort.tool_journal[0].operation_id.clone(),
        operation_kind: OperationKind::Tool,
        cycle_index: 2,
        state: OperationState::Ambiguous,
        risk: "unknown external tool outcome".to_string(),
        idempotency_support: Some(ToolIdempotency::Unknown),
    });
    abort.terminal_result = Some(abort_result.to_dict());
    let abort_revision = abort.revision;
    assert!(store
        .finalize_claimed_checkpoint(abort, "abort-owner", abort_revision)
        .unwrap());
    let abort = store.load_checkpoint(&abort_key).unwrap().unwrap();
    assert_eq!(abort.revision, abort_revision + 1);
    assert!(abort.claim_token.is_none());
    assert_eq!(abort.tool_journal.len(), 1);
    assert_eq!(abort.tool_journal[0].state, OperationState::Ambiguous);
    assert!(abort.terminal_result.as_ref().unwrap()["resume_observation"].is_object());

    let running_event_key = format!("{prefix}-running-event");
    let event = current_event("evt-running");
    let pending = EventOutboxEntry::pending("evt-running", event).unwrap();
    let digest = pending.payload_digest.clone();
    let mut running = minimal_checkpoint(&running_event_key);
    running.event_outbox.push(pending);
    assert!(store.create_checkpoint(running).unwrap());
    let running = store
        .claim_checkpoint(
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
        .record_event_delivery(
            &running_event_key,
            Some("event-owner"),
            running.revision + 1,
            "evt-running",
            &digest,
            cursor.clone(),
        )
        .unwrap());
    assert!(!store
        .record_event_delivery(
            &running_event_key,
            Some("wrong-owner"),
            running.revision,
            "evt-running",
            &digest,
            cursor.clone(),
        )
        .unwrap());
    assert!(!store
        .record_event_delivery(
            &running_event_key,
            Some("event-owner"),
            running.revision,
            "evt-running",
            &"b".repeat(64),
            cursor.clone(),
        )
        .unwrap());
    assert!(store
        .record_event_delivery(
            &running_event_key,
            Some("event-owner"),
            running.revision,
            "evt-running",
            &digest,
            cursor.clone(),
        )
        .unwrap());
    let delivered = store.load_checkpoint(&running_event_key).unwrap().unwrap();
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
    let pending = EventOutboxEntry::pending("evt-terminal", current_event("evt-terminal")).unwrap();
    let digest = pending.payload_digest.clone();
    let mut terminal = minimal_checkpoint(&terminal_event_key);
    terminal.status = CheckpointStatus::Completed;
    let mut result = terminal_result(&terminal, AgentStatus::Completed);
    result.completion_reason = Some(CompletionReason::NoToolFinish);
    result.final_answer = Some("done".to_string());
    terminal.terminal_result = Some(result.to_dict());
    terminal.event_outbox.push(pending);
    let terminal_receipt = terminal.terminal_result.clone();
    assert!(store.create_checkpoint(terminal).unwrap());
    let cursor = delivery_cursor("evt-terminal", 2);
    assert!(!store
        .record_event_delivery(
            &terminal_event_key,
            None,
            1,
            "evt-terminal",
            &digest,
            cursor.clone(),
        )
        .unwrap());
    assert!(!store
        .record_event_delivery(
            &terminal_event_key,
            Some("unexpected-owner"),
            0,
            "evt-terminal",
            &digest,
            cursor.clone(),
        )
        .unwrap());
    assert!(store
        .record_event_delivery(
            &terminal_event_key,
            None,
            0,
            "evt-terminal",
            &digest,
            cursor,
        )
        .unwrap());
    let terminal = store.load_checkpoint(&terminal_event_key).unwrap().unwrap();
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
fn in_memory_store_supports_claimed_finalize_and_event_delivery() {
    exercise_current_store_contract(&InMemoryCheckpointStore::new(), "memory-current");
}

#[test]
fn sqlite_store_supports_claimed_finalize_and_event_delivery() {
    let directory = tempdir().unwrap();
    let store = SqliteCheckpointStore::new(directory.path().join("checkpoint.sqlite3")).unwrap();
    exercise_current_store_contract(&store, "sqlite-current");
}

#[test]
fn store_rejects_run_definition_replacement() {
    let store = InMemoryCheckpointStore::new();
    let checkpoint = checkpoint_from_value(&codec_case("minimal_running"), 262_144).unwrap();
    let key = checkpoint.checkpoint_key.clone();
    assert!(store.create_checkpoint(checkpoint).unwrap());
    let mut claimed = store
        .claim_checkpoint(&key, 1, "owner", 200, 100, ClaimMode::Continue)
        .unwrap()
        .unwrap();
    claimed.run_definition["root_input"] = json!("replacement");
    claimed.run_definition_digest = run_definition_digest(&claimed.run_definition).unwrap();
    let revision = claimed.revision;
    assert!(!store
        .progress_checkpoint(claimed, "owner", revision)
        .unwrap());
}

#[test]
fn canonical_outbox_round_trips_and_delivery_verifies_digest() {
    let checkpoint =
        checkpoint_from_value(&fixture(CODEC_FIXTURE)["canonical_checkpoint"], 262_144).unwrap();
    let placeholder = &checkpoint.event_outbox[0];
    placeholder.verify_payload().unwrap();

    let entry =
        EventOutboxEntry::pending(placeholder.event_id.clone(), placeholder.event.clone()).unwrap();
    entry.verify_payload().unwrap();
}

#[test]
fn redis_keys_match_contract_vectors() {
    let fixture = fixture(STORE_FIXTURE);
    let operations = fixture["operations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|operation| operation["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(operations.contains(&"finalize_claimed"));
    assert!(operations.contains(&"record_event_delivery"));
    for vector in fixture["redis_key_vectors"].as_array().unwrap() {
        let key = vector["checkpoint_key"].as_str().unwrap();
        assert_eq!(RedisCheckpointStore::data_key(key), vector["data_key"]);
        assert_eq!(RedisCheckpointStore::lease_key(key), vector["lease_key"]);
    }
}

#[test]
#[ignore = "requires VV_AGENT_REDIS_URL and a live Redis instance"]
fn redis_store_supports_claimed_finalize_and_event_delivery() {
    let redis_url = std::env::var("VV_AGENT_REDIS_URL").expect("VV_AGENT_REDIS_URL");
    let store = RedisCheckpointStore::new(redis_url).unwrap();
    let prefix = format!("redis-current-{}", uuid::Uuid::new_v4());
    exercise_current_store_contract(&store, &prefix);
}
