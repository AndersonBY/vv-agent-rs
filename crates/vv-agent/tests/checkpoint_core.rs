use std::collections::BTreeMap;
use std::time::SystemTime;

use base64::Engine as _;
use serde_json::{json, Value};
use tempfile::tempdir;
use vv_agent::runtime::checkpoint_codec::{checkpoint_from_value, checkpoint_to_value};
use vv_agent::runtime::state::validate_extension_state_size;
use vv_agent::{
    canonical_json_bytes, checkpoint_from_json, event_payload_digest, model_request_digest,
    operation_request_digest, run_definition_digest, tool_request_digest, AgentResult, AgentStatus,
    CapabilityRef, Checkpoint, CheckpointStatus, CheckpointStore, ClaimMode, CompletionReason,
    CycleRecord, EventCursor, EventOutboxEntry, ExtensionStateEntry, InMemoryCheckpointStore,
    Message, OperationError, OperationJournalEntry, OperationKind, OperationState,
    RedisCheckpointStore, ResumeObservation, RunEvent, SqliteCheckpointStore, ToolArtifactRef,
    ToolIdempotency,
};

const CODEC_FIXTURE: &str = include_str!("fixtures/parity/checkpoint_codec.json");
const DEFINITION_FIXTURE: &str = include_str!("fixtures/parity/run_definition.json");
const JOURNAL_FIXTURE: &str = include_str!("fixtures/parity/operation_journal.json");
const STORE_FIXTURE: &str = include_str!("fixtures/parity/checkpoint_store.json");

#[path = "checkpoint_core/strict.rs"]
mod checkpoint_core_strict;
#[path = "checkpoint_core/store_contract.rs"]
mod store_contract;

fn fixture(raw: &str) -> Value {
    serde_json::from_str(raw).expect("valid parity fixture")
}

fn definition_case(name: &str) -> Value {
    fixture(DEFINITION_FIXTURE)["golden_cases"]
        .as_array()
        .expect("definition golden cases")
        .iter()
        .find(|case| case["name"] == name)
        .unwrap_or_else(|| panic!("missing run-definition golden case {name}"))["definition"]
        .clone()
}

fn decode_pointer_token(token: &str) -> String {
    token.replace("~1", "/").replace("~0", "~")
}

fn pointer_parent_mut<'a>(document: &'a mut Value, pointer: &str) -> (&'a mut Value, String) {
    let (parent_pointer, token) = pointer
        .rsplit_once('/')
        .unwrap_or_else(|| panic!("invalid fixture JSON pointer {pointer}"));
    let parent = if parent_pointer.is_empty() {
        document
    } else {
        document
            .pointer_mut(parent_pointer)
            .unwrap_or_else(|| panic!("missing fixture JSON pointer parent {parent_pointer}"))
    };
    (parent, decode_pointer_token(token))
}

fn remove_pointer(document: &mut Value, pointer: &str) {
    let (parent, token) = pointer_parent_mut(document, pointer);
    let removed = match parent {
        Value::Object(object) => object.remove(&token),
        Value::Array(items) => Some(items.remove(token.parse::<usize>().expect("array index"))),
        _ => panic!("fixture JSON pointer parent is not a container: {pointer}"),
    };
    assert!(
        removed.is_some(),
        "fixture JSON pointer is missing: {pointer}"
    );
}

fn set_pointer(document: &mut Value, pointer: &str, value: Value) {
    let (parent, token) = pointer_parent_mut(document, pointer);
    match parent {
        Value::Object(object) => {
            object.insert(token, value);
        }
        Value::Array(items) => {
            items[token.parse::<usize>().expect("array index")] = value;
        }
        _ => panic!("fixture JSON pointer parent is not a container: {pointer}"),
    }
}

fn apply_definition_mutation(definition: &mut Value, mutation: &Value) {
    if let Some(fields) = mutation.get("add").and_then(Value::as_object) {
        definition
            .as_object_mut()
            .expect("definition object")
            .extend(fields.clone());
    }
    if let Some(pointer) = mutation.get("remove_json_pointer").and_then(Value::as_str) {
        remove_pointer(definition, pointer);
    }
    for operation in ["replace_json_pointer", "add_json_pointer"] {
        if let Some(pointer) = mutation.get(operation).and_then(Value::as_str) {
            set_pointer(definition, pointer, mutation["value"].clone());
        }
    }
    if let Some(pointer) = mutation.get("append_json_pointer").and_then(Value::as_str) {
        definition
            .pointer_mut(pointer)
            .and_then(Value::as_array_mut)
            .unwrap_or_else(|| panic!("fixture append target is not an array: {pointer}"))
            .push(mutation["value"].clone());
    }
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

fn current_codec_case(name: &str) -> Value {
    let mut payload = codec_case(name);
    payload["run_definition_schema"] = json!("vv-agent.run-definition.v5");
    payload["run_definition"]["schema_version"] = json!("vv-agent.run-definition.v5");
    payload["run_definition"]["runtime_controls"]["microcompaction_policy"] = json!({
        "trigger_ratio": 0.75,
        "target_ratio": 0.60,
        "keep_recent_cycles": 3,
        "min_result_chars": 500
    });
    payload["run_definition_digest"] =
        json!(run_definition_digest(&payload["run_definition"]).expect("current definition"));
    payload
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
            payload["idempotency_key"].as_str(),
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
fn run_definition_v5_rejects_invalid_microcompaction_policy_shapes() {
    let fixture = fixture(DEFINITION_FIXTURE);
    let definition = fixture["golden_cases"][0]["definition"].clone();

    let mut missing = definition.clone();
    missing["runtime_controls"]
        .as_object_mut()
        .expect("runtime controls")
        .remove("microcompaction_policy");
    assert_eq!(
        run_definition_digest(&missing).unwrap_err().code(),
        "checkpoint_definition_invalid"
    );

    let mut unknown = definition.clone();
    unknown["runtime_controls"]["microcompaction_policy"]["future_behavior"] = json!(true);
    assert_eq!(
        run_definition_digest(&unknown).unwrap_err().code(),
        "checkpoint_definition_invalid"
    );

    let mut invalid_ratio = definition;
    invalid_ratio["runtime_controls"]["microcompaction_policy"]["target_ratio"] = json!(0.75);
    assert_eq!(
        run_definition_digest(&invalid_ratio).unwrap_err().code(),
        "checkpoint_definition_invalid"
    );
}

#[test]
fn run_definition_v5_rejects_all_json_representable_fixture_invalid_cases() {
    let fixture = fixture(DEFINITION_FIXTURE);
    let producer_only = [
        "unstable_process_local_capability",
        "unstable_process_local_hook",
        "unstable_process_local_after_cycle_hook",
        "behavior_metadata_without_reference",
        "non_finite_number",
    ];
    let mut covered = Vec::new();

    for case in fixture["invalid_cases"]
        .as_array()
        .expect("definition invalid cases")
    {
        let name = case["name"].as_str().expect("invalid case name");
        let expected = case["error_code"].as_str().expect("invalid case code");
        let definition =
            if let Some(base_name) = case.get("base_golden_case").and_then(Value::as_str) {
                let mut definition = definition_case(base_name);
                apply_definition_mutation(&mut definition, &case["mutation"]);
                definition
            } else {
                match name {
                    "extra_header_case_collision" => {
                        let mut definition = definition_case("full_unicode_float_and_capabilities");
                        definition["model"]["settings"]["extra_headers"]["Authorization"] =
                            json!("second");
                        definition
                    }
                    "unsafe_integer" => {
                        let mut definition = definition_case("full_unicode_float_and_capabilities");
                        let generated = &case["generated_input"];
                        set_pointer(
                            &mut definition,
                            generated["json_pointer"].as_str().expect("integer pointer"),
                            generated["value"].clone(),
                        );
                        definition
                    }
                    producer_only_name if producer_only.contains(&producer_only_name) => continue,
                    other => panic!("unhandled run-definition invalid fixture case {other}"),
                }
            };

        let error = run_definition_digest(&definition)
            .expect_err(&format!("{name} unexpectedly produced a valid digest"));
        assert_eq!(error.code(), expected, "{name}: {error:?}");
        covered.push(name);
    }

    assert_eq!(
        covered.len() + producer_only.len(),
        fixture["invalid_cases"].as_array().unwrap().len()
    );
    assert!(serde_json::Number::from_f64(f64::NAN).is_none());
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
fn checkpoint_codec_round_trips_every_canonical_valid_case() {
    for case in fixture(CODEC_FIXTURE)["valid_cases"]
        .as_array()
        .expect("valid cases")
    {
        let name = case["name"].as_str().expect("valid case name");
        let checkpoint = checkpoint_from_value(&case["payload"], 262_144)
            .unwrap_or_else(|error| panic!("{name}: valid checkpoint rejected: {error:?}"));
        assert_eq!(
            checkpoint_to_value(&checkpoint, 262_144).unwrap(),
            case["payload"],
            "{name}: canonical checkpoint changed during round trip"
        );
    }
}

#[test]
fn checkpoint_codec_maps_cancel_requested_shape_errors_to_status_invalid() {
    let fixture = fixture(CODEC_FIXTURE);
    let expected_error = fixture["invalid_cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "cancel_requested_not_boolean")
        .and_then(|case| case["error"].as_str())
        .expect("cancel_requested fixture error");

    let mut non_boolean = codec_case("minimal_running");
    non_boolean["cancel_requested"] = json!("yes");
    assert_eq!(
        checkpoint_from_value(&non_boolean, 262_144)
            .unwrap_err()
            .code(),
        expected_error
    );
}

#[test]
fn checkpoint_round_trips_message_artifact_ref() {
    let artifact_ref = ToolArtifactRef {
        path: ".vv-agent/artifacts/checkpoint/call.txt".to_string(),
        media_type: "text/plain".to_string(),
        encoding: "utf-8".to_string(),
        size_bytes: 17,
        sha256: "b".repeat(64),
    };
    let mut message = Message::tool("bounded preview", "call");
    message.artifact_ref = Some(artifact_ref.clone());
    let mut payload = current_codec_case("minimal_running");
    payload["messages"] = json!([message.to_dict()]);

    let checkpoint = checkpoint_from_value(&payload, 262_144).expect("checkpoint");
    let encoded = checkpoint_to_value(&checkpoint, 262_144).expect("checkpoint wire");
    let restored = checkpoint_from_value(&encoded, 262_144).expect("restored checkpoint");

    assert_eq!(restored.messages[0].artifact_ref, Some(artifact_ref));
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
            if mutation.get("operation").and_then(Value::as_str) == Some("remove") {
                if let Some(field) = mutation
                    .get("path")
                    .and_then(Value::as_array)
                    .and_then(|path| path.last())
                    .and_then(Value::as_str)
                {
                    entry.as_object_mut().unwrap().remove(field);
                }
            }
            if let Some(replacements) = mutation.get("replace").and_then(Value::as_object) {
                for (field, value) in replacements {
                    if let Some((parent, child)) = field.split_once('.') {
                        entry[parent][child] = value.clone();
                    } else {
                        entry[field] = value.clone();
                    }
                }
            }
            if let Some(additions) = mutation.get("add").and_then(Value::as_object) {
                entry.as_object_mut().unwrap().extend(additions.clone());
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
    assert!(matches!(
        store
            .renew_checkpoint_claim(key, "owner-b", 400, 250)
            .unwrap(),
        vv_agent::CheckpointRenewalOutcome::Renewed { .. }
    ));
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

fn assert_initial_create_rejected_without_write(
    store: &dyn CheckpointStore,
    checkpoint: Checkpoint,
) {
    let key = checkpoint.checkpoint_key.clone();
    let error = store
        .create_checkpoint(checkpoint)
        .expect_err("non-initial checkpoint must be rejected");
    assert_eq!(error.code(), "checkpoint_initial_invalid");
    assert!(store
        .load_checkpoint(&key)
        .expect("load after rejected create")
        .is_none());

    let valid = minimal_checkpoint(&key);
    assert!(store
        .create_checkpoint(valid)
        .expect("rejected create must not reserve the key"));
    assert!(store
        .load_checkpoint(&key)
        .expect("load valid checkpoint")
        .is_some());
}

fn invalid_initial_checkpoints(prefix: &str) -> Vec<Checkpoint> {
    let mut revision = minimal_checkpoint(&format!("{prefix}-revision"));
    revision.revision = 1;

    let mut claimed = minimal_checkpoint(&format!("{prefix}-claimed"));
    claimed.claim_token = Some("create-claim".to_string());
    claimed.claimed_cycle = Some(1);
    claimed.lease_expires_at_ms = Some(100);

    let mut terminal = minimal_checkpoint(&format!("{prefix}-terminal"));
    terminal.status = CheckpointStatus::Completed;
    terminal.terminal_result = Some(terminal_result(&terminal, AgentStatus::Completed).to_dict());

    let mut journal = minimal_checkpoint(&format!("{prefix}-journal"));
    journal.tool_journal = vec![journal_case("tool_started")];

    let mut outbox = minimal_checkpoint(&format!("{prefix}-outbox"));
    outbox.event_outbox.push(
        EventOutboxEntry::pending("evt-invalid-create", current_event("evt-invalid-create"))
            .unwrap(),
    );

    vec![revision, claimed, terminal, journal, outbox]
}

#[test]
fn memory_store_rejects_non_initial_create_without_writes() {
    let store = InMemoryCheckpointStore::new();
    for checkpoint in invalid_initial_checkpoints("memory-create") {
        assert_initial_create_rejected_without_write(&store, checkpoint);
    }
}

#[test]
fn sqlite_store_rejects_non_initial_create_without_writes() {
    let directory = tempdir().unwrap();
    let store = SqliteCheckpointStore::new(directory.path().join("checkpoint.sqlite3")).unwrap();
    for checkpoint in invalid_initial_checkpoints("sqlite-create") {
        assert_initial_create_rejected_without_write(&store, checkpoint);
    }
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
    failure_result.error = Some(vv_agent::AgentResultError::new(
        "provider_rejected",
        "provider_rejected",
        false,
    ));
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
    let mut abort_seed = minimal_checkpoint(&abort_key);
    abort_seed.cycle_index = 0;
    assert!(store.create_checkpoint(abort_seed).unwrap());
    let mut abort = store
        .claim_checkpoint(&abort_key, 1, "abort-owner", 400, 300, ClaimMode::Continue)
        .unwrap()
        .unwrap();
    abort.tool_journal = vec![journal_case("tool_started")];
    abort.tool_journal[0].mark_ambiguous().unwrap();
    abort.status = CheckpointStatus::Failed;
    let mut abort_result = terminal_result(&abort, AgentStatus::Failed);
    abort_result.completion_reason = Some(CompletionReason::Failed);
    abort_result.error = Some(vv_agent::AgentResultError::new(
        "operator_abort_with_unknown_outcome",
        "Operator accepted that the external outcome is unknown.",
        false,
    ));
    abort_result.resume_observations = vec![ResumeObservation {
        operation_id: abort.tool_journal[0].operation_id.clone(),
        operation_kind: OperationKind::Tool,
        cycle_index: abort.tool_journal[0].cycle_index,
        state: OperationState::Ambiguous,
        risk: "unknown external tool outcome".to_string(),
        idempotency_support: Some(ToolIdempotency::Unknown),
    }];
    abort.terminal_result = Some(abort_result.to_dict());
    let abort_revision = abort.revision;
    assert!(store
        .finalize_claimed_checkpoint(abort, "abort-owner", abort_revision)
        .unwrap());
    let abort = store.load_checkpoint(&abort_key).unwrap().unwrap();
    assert_eq!(abort.revision, abort_revision + 1);
    assert!(abort.claim_token.is_none());
    assert_eq!(abort.tool_journal.len(), 1);
    assert_eq!(abort.tool_journal[0].state, OperationState::Failed);
    assert_eq!(
        abort.tool_journal[0]
            .error
            .as_ref()
            .map(|error| error.code.as_str()),
        Some("tool_cancelled")
    );
    assert!(
        abort.terminal_result.as_ref().unwrap()["resume_observations"]
            .as_array()
            .is_some_and(|values| values.len() == 1)
    );

    let running_event_key = format!("{prefix}-running-event");
    let event = current_event("evt-running");
    let pending = EventOutboxEntry::pending("evt-running", event).unwrap();
    let digest = pending.payload_digest.clone();
    let mut running = minimal_checkpoint(&running_event_key);
    assert!(store.create_checkpoint(running.clone()).unwrap());
    running = store
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
    running.event_outbox.push(pending);
    let running_revision = running.revision;
    assert!(store
        .progress_checkpoint(running, "event-owner", running_revision)
        .unwrap());
    let running = store.load_checkpoint(&running_event_key).unwrap().unwrap();
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
    assert!(store.create_checkpoint(terminal.clone()).unwrap());
    terminal.status = CheckpointStatus::Completed;
    let mut result = terminal_result(&terminal, AgentStatus::Completed);
    result.completion_reason = Some(CompletionReason::NoToolFinish);
    result.final_answer = Some("done".to_string());
    terminal.terminal_result = Some(result.to_dict());
    terminal.event_outbox.push(pending);
    let terminal_receipt = terminal.terminal_result.clone();
    assert!(store.finalize_checkpoint(terminal, 0).unwrap());
    let terminal = store.load_checkpoint(&terminal_event_key).unwrap().unwrap();
    let cursor = delivery_cursor("evt-terminal", 2);
    assert!(!store
        .record_event_delivery(
            &terminal_event_key,
            None,
            terminal.revision + 1,
            "evt-terminal",
            &digest,
            cursor.clone(),
        )
        .unwrap());
    assert!(!store
        .record_event_delivery(
            &terminal_event_key,
            Some("unexpected-owner"),
            terminal.revision,
            "evt-terminal",
            &digest,
            cursor.clone(),
        )
        .unwrap());
    assert!(store
        .record_event_delivery(
            &terminal_event_key,
            None,
            terminal.revision,
            "evt-terminal",
            &digest,
            cursor,
        )
        .unwrap());
    let terminal = store.load_checkpoint(&terminal_event_key).unwrap().unwrap();
    assert_eq!(terminal.revision, 2);
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
