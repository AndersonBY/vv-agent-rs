use std::time::SystemTime;

use serde_json::Value;
use tempfile::tempdir;
use vv_agent::runtime::checkpoint_codec::checkpoint_from_value;
use vv_agent::{
    run_definition_digest, tool_request_digest, CapabilityRef, Checkpoint, CheckpointStore,
    ClaimMode, EventCursor, EventOutboxEntry, InMemoryCheckpointStore, OperationError,
    OperationJournalEntry, OperationKind, OperationState, RedisCheckpointStore, ResumeObservation,
    RunEvent, RunEventPayload, SqliteCheckpointStore, ToolExecutionResult, ToolIdempotency,
};

const CODEC_FIXTURE: &str = include_str!("fixtures/parity/checkpoint_codec.json");

fn minimal_checkpoint(key: &str) -> Checkpoint {
    let mut fixture: Value = serde_json::from_str(CODEC_FIXTURE).expect("codec fixture");
    let payload = fixture["valid_cases"]
        .as_array_mut()
        .expect("valid cases")
        .iter_mut()
        .find(|case| case["name"] == "minimal_running")
        .expect("minimal running case")["payload"]
        .clone();
    let mut payload = payload;
    payload["checkpoint_key"] = Value::String(key.to_string());
    payload["run_definition_schema"] = Value::String("vv-agent.run-definition.v5".to_string());
    payload["run_definition"]["schema_version"] =
        Value::String("vv-agent.run-definition.v5".to_string());
    payload["run_definition"]["runtime_controls"]["microcompaction_policy"] = serde_json::json!({
        "trigger_ratio": 0.75,
        "target_ratio": 0.60,
        "keep_recent_cycles": 3,
        "min_result_chars": 500
    });
    payload["run_definition_digest"] =
        serde_json::json!(run_definition_digest(&payload["run_definition"]).expect("definition"));
    checkpoint_from_value(&payload, 262_144).expect("checkpoint")
}

fn delivery_cursor(event_id: &str) -> EventCursor {
    EventCursor::new(
        CapabilityRef::new("events.tenant", "1").expect("capability ref"),
        serde_json::json!({"event_id": event_id}),
        Some(event_id.to_string()),
    )
}

fn caller_event() -> EventOutboxEntry {
    let event = RunEvent::run_started(
        "run-progress-event-merge",
        "trace-progress-event-merge",
        "assistant",
        "resume",
    )
    .with_event_id("caller-progress-event")
    .expect("caller event");
    EventOutboxEntry::pending(
        "caller-progress-event",
        serde_json::to_value(event).expect("caller event wire"),
    )
    .expect("caller outbox entry")
}

fn exercise_progress_merges_authoritative_events(store: &dyn CheckpointStore, prefix: &str) {
    let key = format!("{prefix}-progress-event-merge");
    let cancel_event = RunEvent::new(
        "run-progress-event-merge",
        "trace-progress-event-merge",
        "vv-agent",
        None,
        RunEventPayload::RunStateChanged {
            state: "running".to_string(),
        },
    )
    .with_event_id("controller-cancel-event")
    .expect("cancel event");
    let mut cancel_event = serde_json::to_value(cancel_event).expect("cancel event wire");
    cancel_event["cancel_requested"] = serde_json::json!({"from": false, "to": true});
    let cancel_entry = EventOutboxEntry::pending("controller-cancel-event", cancel_event)
        .expect("cancel outbox entry");
    let initial = minimal_checkpoint(&key);
    assert!(store.create_checkpoint(initial).expect("create checkpoint"));

    let claimed = store
        .claim_checkpoint(&key, 1, "progress-owner", 700, 600, ClaimMode::Continue)
        .expect("claim checkpoint")
        .expect("claimed checkpoint");
    let mut staged = claimed.clone();
    staged.event_outbox.push(cancel_entry);
    assert!(store
        .progress_checkpoint(staged, "progress-owner", claimed.revision)
        .expect("stage controller event"));
    let claimed = store
        .load_checkpoint(&key)
        .expect("load staged checkpoint")
        .expect("staged checkpoint");
    let stale = claimed.clone();
    let cancel_digest = claimed.event_outbox[0].payload_digest.clone();
    let cancel_cursor = delivery_cursor("controller-cancel-event");
    assert!(store
        .record_event_delivery(
            &key,
            Some("progress-owner"),
            claimed.revision,
            "controller-cancel-event",
            &cancel_digest,
            cancel_cursor.clone(),
        )
        .expect("deliver controller event"));

    let authoritative = store
        .load_checkpoint(&key)
        .expect("load authoritative")
        .expect("authoritative checkpoint");
    let mut caller = stale;
    caller.event_outbox.push(caller_event());
    caller.revision = authoritative.revision;
    assert!(store
        .progress_checkpoint(caller, "progress-owner", authoritative.revision)
        .expect("progress checkpoint"));

    let progressed = store
        .load_checkpoint(&key)
        .expect("load progressed")
        .expect("progressed checkpoint");
    assert_eq!(
        progressed
            .event_outbox
            .iter()
            .map(|entry| entry.event_id.as_str())
            .collect::<Vec<_>>(),
        ["controller-cancel-event", "caller-progress-event"]
    );
    assert_eq!(progressed.event_outbox[0].state, "delivered");
    assert_eq!(progressed.event_cursor, Some(cancel_cursor));
}

fn exercise_planned_failure_can_commit(store: &dyn CheckpointStore, prefix: &str) {
    let key = format!("{prefix}-planned-failure");
    let initial = minimal_checkpoint(&key);
    assert!(store.create_checkpoint(initial).expect("create checkpoint"));
    let claimed = store
        .claim_checkpoint(
            &key,
            1,
            "planned-failure-owner",
            700,
            600,
            ClaimMode::Continue,
        )
        .expect("claim checkpoint")
        .expect("claimed checkpoint");

    let request_digest = tool_request_digest(
        "call_missing",
        "missing_tool",
        &serde_json::json!({}),
        Some("idem-missing"),
    )
    .expect("request digest");
    let mut planned = claimed.clone();
    planned.tool_journal.push(OperationJournalEntry::tool(
        "tool_cycle_1_call_missing",
        1,
        1,
        request_digest.clone(),
        "call_missing",
        "missing_tool",
        serde_json::Map::new(),
        Some("idem-missing".to_string()),
        ToolIdempotency::Unknown,
    ));
    assert!(store
        .progress_checkpoint(planned, "planned-failure-owner", claimed.revision)
        .expect("persist planned entry"));
    let planned = store
        .load_checkpoint(&key)
        .expect("load planned checkpoint")
        .expect("planned checkpoint");

    let result = ToolExecutionResult::error("call_missing", "tool lookup failed")
        .with_error_code("tool_not_found");
    let identity_key = vv_agent::checkpoint::tool_receipt_identity_key(
        &key,
        "tool_cycle_1_call_missing",
        1,
        "call_missing",
        &request_digest,
    )
    .expect("identity key");
    let result_digest = vv_agent::checkpoint::tool_result_digest(&result).expect("result digest");
    let mut failed = planned.clone();
    let entry = failed.tool_journal.first_mut().expect("planned tool entry");
    entry.state = OperationState::Failed;
    entry.identity_key = Some(identity_key.clone());
    entry.result_digest = Some(result_digest.clone());
    entry.result = Some(result.to_dict());
    entry.error = Some(OperationError::new(
        "tool_not_found",
        "tool lookup failed",
        false,
    ));
    entry.validate().expect("canonical failed entry");
    assert!(store
        .progress_checkpoint(failed, "planned-failure-owner", planned.revision)
        .expect("persist failed entry"));

    let failed = store
        .load_checkpoint(&key)
        .expect("load failed checkpoint")
        .expect("failed checkpoint");
    let entry = failed.tool_journal.first().expect("failed tool entry");
    assert_eq!(entry.state, OperationState::Failed);
    assert_eq!(entry.identity_key.as_deref(), Some(identity_key.as_str()));
    assert_eq!(entry.result_digest.as_deref(), Some(result_digest.as_str()));
    assert_eq!(
        entry.error.as_ref().map(|error| error.code.as_str()),
        Some("tool_not_found")
    );
    assert_eq!(entry.result.as_ref(), Some(&result.to_dict()));
    let mut ready_to_commit = failed.clone();
    ready_to_commit.cycle_index = 1;
    assert!(store
        .commit_checkpoint(ready_to_commit, "planned-failure-owner", failed.revision)
        .expect("commit closed failed cycle"));
    let committed = store
        .load_checkpoint(&key)
        .expect("load committed checkpoint")
        .expect("committed checkpoint");
    assert!(committed.tool_journal.is_empty());
    assert!(committed.claim_token.is_none());
}

fn exercise_unknown_receipt_replay_preserves_digest_and_zero_writes(
    store: &dyn CheckpointStore,
    prefix: &str,
) {
    let key = format!("{prefix}-unknown-receipt-replay");
    assert!(store
        .create_checkpoint(minimal_checkpoint(&key))
        .expect("create checkpoint"));
    let claimed = store
        .claim_checkpoint(
            &key,
            1,
            "unknown-receipt-owner",
            700,
            600,
            ClaimMode::Continue,
        )
        .expect("claim checkpoint")
        .expect("claimed checkpoint");
    let request_digest = tool_request_digest(
        "call-unknown",
        "unsafe_write",
        &serde_json::json!({"value": 42}),
        Some("idem-unknown"),
    )
    .expect("request digest");
    let mut started = claimed.clone();
    let mut entry = OperationJournalEntry::tool(
        "tool_cycle_1_call_unknown",
        1,
        1,
        request_digest.clone(),
        "call-unknown",
        "unsafe_write",
        serde_json::Map::from_iter([("value".to_string(), serde_json::json!(42))]),
        Some("idem-unknown".to_string()),
        ToolIdempotency::Unknown,
    );
    entry
        .transition_to(OperationState::Started)
        .expect("start tool operation");
    started.tool_journal.push(entry);
    assert!(store
        .progress_checkpoint(started, "unknown-receipt-owner", claimed.revision)
        .expect("persist started operation"));
    let claimed = store
        .load_checkpoint(&key)
        .expect("load started checkpoint")
        .expect("started checkpoint");

    let result = ToolExecutionResult::error("call-unknown", "The tool outcome is unknown.")
        .with_error_code("tool_outcome_unknown");
    let result_digest = vv_agent::checkpoint::tool_result_digest(&result).expect("result digest");
    let error = store
        .record_tool_receipt(
            claimed.clone(),
            "tool_cycle_1_call_unknown",
            1,
            "call-unknown",
            &request_digest,
            result.clone(),
            "unknown-receipt-owner",
            claimed.revision,
            1,
        )
        .expect_err("started operation without observation is not definitive");
    assert_eq!(error.code(), "checkpoint_journal_integrity_mismatch");
    assert_eq!(
        store.load_checkpoint(&key).unwrap().unwrap().revision,
        claimed.revision
    );

    let mut ambiguous = claimed;
    ambiguous.tool_journal[0]
        .transition_to(OperationState::Ambiguous)
        .unwrap();
    assert!(store
        .progress_checkpoint(
            ambiguous.clone(),
            "unknown-receipt-owner",
            ambiguous.revision
        )
        .unwrap());
    let mut claimed = store.load_checkpoint(&key).unwrap().unwrap();
    let observation = ResumeObservation {
        operation_id: "tool_cycle_1_call_unknown".to_string(),
        operation_kind: OperationKind::Tool,
        cycle_index: 1,
        state: OperationState::Ambiguous,
        risk: "unknown_tool_side_effect".to_string(),
        idempotency_support: Some(ToolIdempotency::Unknown),
    };
    claimed.tool_journal[0].resume_observation = Some(observation.clone());
    for source_case in ["missing_observation", "wrong_cycle", "wrong_observation"] {
        let mut invalid = claimed.clone();
        let source = &mut invalid.tool_journal[0];
        match source_case {
            "missing_observation" => source.resume_observation = None,
            "wrong_cycle" => source.cycle_index += 1,
            _ => {
                source.resume_observation.as_mut().unwrap().operation_id =
                    "wrong-operation".to_string()
            }
        }
        let error = store
            .record_tool_receipt(
                invalid,
                "tool_cycle_1_call_unknown",
                1,
                "call-unknown",
                &request_digest,
                result.clone(),
                "unknown-receipt-owner",
                claimed.revision,
                1,
            )
            .expect_err("incomplete observation");
        assert_eq!(
            error.code(),
            "checkpoint_journal_integrity_mismatch",
            "{source_case}"
        );
        assert_eq!(
            store.load_checkpoint(&key).unwrap().unwrap().revision,
            claimed.revision
        );
    }
    assert!(store
        .record_tool_receipt(
            claimed.clone(),
            "tool_cycle_1_call_unknown",
            1,
            "call-unknown",
            &request_digest,
            result.clone(),
            "unknown-receipt-owner",
            claimed.revision,
            1,
        )
        .expect("record unknown receipt"));
    let persisted = store
        .load_checkpoint(&key)
        .expect("load failed receipt")
        .expect("failed receipt");
    let entry = persisted.tool_journal.first().expect("failed tool entry");
    assert_eq!(entry.state, OperationState::Failed);
    assert_eq!(entry.result_digest.as_deref(), Some(result_digest.as_str()));
    assert_eq!(entry.resume_observation, Some(observation));
    assert_eq!(
        entry.error.as_ref().map(|error| error.code.as_str()),
        Some("tool_outcome_unknown")
    );
    let revision = persisted.revision;
    let events = persisted
        .event_outbox
        .iter()
        .map(|event| (event.event_id.clone(), event.payload_digest.clone()))
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 1);

    assert!(store
        .record_tool_receipt(
            claimed.clone(),
            "tool_cycle_1_call_unknown",
            1,
            "call-unknown",
            &request_digest,
            result,
            "unknown-receipt-owner",
            claimed.revision,
            1,
        )
        .expect("replay unknown receipt"));
    let replayed = store
        .load_checkpoint(&key)
        .expect("load replayed receipt")
        .expect("replayed receipt");
    assert_eq!(replayed.revision, revision);
    assert_eq!(
        replayed
            .event_outbox
            .iter()
            .map(|event| (event.event_id.clone(), event.payload_digest.clone()))
            .collect::<Vec<_>>(),
        events
    );

    let conflict = ToolExecutionResult::error("call-unknown", "A different unknown outcome.")
        .with_error_code("tool_outcome_unknown");
    let error = store
        .record_tool_receipt(
            claimed.clone(),
            "tool_cycle_1_call_unknown",
            1,
            "call-unknown",
            &request_digest,
            conflict,
            "unknown-receipt-owner",
            claimed.revision,
            1,
        )
        .expect_err("conflicting receipt must be rejected");
    assert_eq!(error.code(), "tool_receipt_conflict");
    let unchanged = store
        .load_checkpoint(&key)
        .expect("load unchanged receipt")
        .expect("unchanged receipt");
    assert_eq!(unchanged.revision, revision);
    assert_eq!(
        unchanged
            .event_outbox
            .iter()
            .map(|event| (event.event_id.clone(), event.payload_digest.clone()))
            .collect::<Vec<_>>(),
        events
    );
}

fn exercise_tool_receipt_preconditions_are_typed_and_zero_write(
    store: &dyn CheckpointStore,
    prefix: &str,
) {
    let key = format!("{prefix}-tool-receipt-preconditions");
    assert!(store
        .create_checkpoint(minimal_checkpoint(&key))
        .expect("create checkpoint"));
    let claimed = store
        .claim_checkpoint(&key, 1, "receipt-owner", 700, 600, ClaimMode::Continue)
        .expect("claim checkpoint")
        .expect("claimed checkpoint");
    let request_digest = tool_request_digest(
        "call-precondition",
        "unsafe_write",
        &serde_json::json!({}),
        Some("idem-precondition"),
    )
    .expect("request digest");
    let mut started = claimed.clone();
    let mut entry = OperationJournalEntry::tool(
        "tool_cycle_1_call_precondition",
        1,
        1,
        request_digest.clone(),
        "call-precondition",
        "unsafe_write",
        serde_json::Map::new(),
        Some("idem-precondition".to_string()),
        ToolIdempotency::Unknown,
    );
    entry
        .transition_to(OperationState::Started)
        .expect("start tool operation");
    started.tool_journal.push(entry);
    assert!(store
        .progress_checkpoint(started, "receipt-owner", claimed.revision)
        .expect("persist started operation"));
    let current = store
        .load_checkpoint(&key)
        .expect("load started checkpoint")
        .expect("started checkpoint");
    let result = ToolExecutionResult::error("call-precondition", "unknown")
        .with_error_code("tool_outcome_unknown");
    let snapshot = |label: &str| {
        let checkpoint = store
            .load_checkpoint(&key)
            .expect(label)
            .expect("checkpoint");
        (
            checkpoint.revision,
            checkpoint.tool_journal,
            checkpoint.event_outbox,
        )
    };
    let before = snapshot("load before missing claim");
    let error = store
        .record_tool_receipt(
            current.clone(),
            "tool_cycle_1_call_precondition",
            1,
            "call-precondition",
            &request_digest,
            result.clone(),
            "",
            current.revision,
            1,
        )
        .expect_err("missing claim must be rejected");
    assert_eq!(error.code(), "checkpoint_claim_required");
    assert_eq!(snapshot("load after missing claim"), before);

    let before = snapshot("load before wrong claim");
    let error = store
        .record_tool_receipt(
            current.clone(),
            "tool_cycle_1_call_precondition",
            1,
            "call-precondition",
            &request_digest,
            result.clone(),
            "wrong-owner",
            current.revision,
            1,
        )
        .expect_err("wrong claim must be rejected");
    assert_eq!(error.code(), "checkpoint_claim_conflict");
    assert_eq!(snapshot("load after wrong claim"), before);

    let before = snapshot("load before stale revision");
    let error = store
        .record_tool_receipt(
            current,
            "tool_cycle_1_call_precondition",
            1,
            "call-precondition",
            &request_digest,
            result,
            "receipt-owner",
            before.0.saturating_sub(1),
            1,
        )
        .expect_err("stale revision must be rejected");
    assert_eq!(error.code(), "checkpoint_revision_conflict");
    assert_eq!(snapshot("load after stale revision"), before);
}

#[test]
fn in_memory_progress_preserves_controller_events_and_merges_new_events() {
    exercise_progress_merges_authoritative_events(
        &InMemoryCheckpointStore::new(),
        "memory-current",
    );
}

#[test]
fn sqlite_progress_preserves_controller_events_and_merges_new_events() {
    let directory = tempdir().expect("tempdir");
    let store = SqliteCheckpointStore::new(directory.path().join("checkpoint.sqlite3"))
        .expect("sqlite store");
    exercise_progress_merges_authoritative_events(&store, "sqlite-current");
}

#[test]
fn in_memory_planned_failure_persists_identity_and_commits() {
    exercise_planned_failure_can_commit(&InMemoryCheckpointStore::new(), "memory-current");
}

#[test]
fn sqlite_planned_failure_persists_identity_and_commits() {
    let directory = tempdir().expect("tempdir");
    let store = SqliteCheckpointStore::new(directory.path().join("checkpoint.sqlite3"))
        .expect("sqlite store");
    exercise_planned_failure_can_commit(&store, "sqlite-current");
}

#[test]
fn in_memory_unknown_receipt_replay_preserves_digest_and_zero_writes() {
    exercise_unknown_receipt_replay_preserves_digest_and_zero_writes(
        &InMemoryCheckpointStore::new(),
        "memory-current",
    );
}

#[test]
fn sqlite_unknown_receipt_replay_preserves_digest_and_zero_writes() {
    let directory = tempdir().expect("tempdir");
    let store = SqliteCheckpointStore::new(directory.path().join("checkpoint.sqlite3"))
        .expect("sqlite store");
    exercise_unknown_receipt_replay_preserves_digest_and_zero_writes(&store, "sqlite-current");
}

#[test]
fn in_memory_tool_receipt_preconditions_are_typed_and_zero_write() {
    exercise_tool_receipt_preconditions_are_typed_and_zero_write(
        &InMemoryCheckpointStore::new(),
        "memory-current",
    );
}

#[test]
fn sqlite_tool_receipt_preconditions_are_typed_and_zero_write() {
    let directory = tempdir().expect("tempdir");
    let store = SqliteCheckpointStore::new(directory.path().join("checkpoint.sqlite3"))
        .expect("sqlite store");
    exercise_tool_receipt_preconditions_are_typed_and_zero_write(&store, "sqlite-current");
}

#[test]
#[ignore = "requires VV_AGENT_TEST_REDIS_URL and a live Redis instance"]
fn redis_unknown_receipt_replay_preserves_digest_and_zero_writes() {
    let redis_url = std::env::var("VV_AGENT_TEST_REDIS_URL").expect("VV_AGENT_TEST_REDIS_URL");
    let store = RedisCheckpointStore::new(&redis_url).expect("redis store");
    let suffix = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let prefix = format!("redis-unknown-receipt-{}-{suffix}", std::process::id());
    let key = format!("{prefix}-unknown-receipt-replay");
    store
        .delete_checkpoint(&key)
        .expect("clean stale checkpoint");
    exercise_unknown_receipt_replay_preserves_digest_and_zero_writes(&store, &prefix);
    store
        .delete_checkpoint(&key)
        .expect("clean test checkpoint");
}
