use super::*;
use vv_agent::{ModelCallOperation, ModelCallRecord, ModelCallStatus, TokenUsage};

#[path = "history_wire_metrics.rs"]
mod wire_metrics;

fn completed_cycle(checkpoint: &mut Checkpoint, index: u32) {
    checkpoint.cycles.push(CycleRecord {
        index,
        assistant_message: "x".repeat(2048),
        tool_calls: Vec::new(),
        tool_results: Vec::new(),
        memory_compacted: false,
    });
    checkpoint.model_calls.push(ModelCallRecord {
        call_id: format!("call-{index}"),
        operation_id: format!("operation-{index}"),
        attempt: 1,
        operation: ModelCallOperation::AgentCycle,
        cycle_index: index,
        backend: "test".to_string(),
        model: "model".to_string(),
        status: ModelCallStatus::Completed,
        usage: TokenUsage {
            input_tokens: Some(u64::from(index)),
            ..TokenUsage::default()
        },
        error_code: None,
    });
    checkpoint.cycle_index = u64::from(index);
}

fn claim(store: &dyn CheckpointStore, key: &str, index: u64) -> Checkpoint {
    let now = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    store
        .claim_checkpoint(key, index, "owner", now + 60_000, now, ClaimMode::Continue)
        .unwrap()
        .unwrap()
}

fn exercise_history(store: &dyn CheckpointStore, key: &str) {
    store.delete_checkpoint(key).unwrap();
    assert!(store.create_checkpoint(minimal_checkpoint(key)).unwrap());
    let mut late_sizes = Vec::new();
    for index in 1..=48 {
        let mut candidate = claim(store, key, index);
        completed_cycle(&mut candidate, index as u32);
        let revision = candidate.revision;
        assert!(!store
            .commit_checkpoint(candidate.clone(), "wrong-owner", revision)
            .unwrap());
        let before = store.load_checkpoint(key).unwrap().unwrap();
        assert_eq!(before.history.cycle_count, index.saturating_sub(2));
        assert!(store
            .commit_checkpoint(candidate.clone(), "owner", revision)
            .unwrap());
        assert!(!store
            .commit_checkpoint(candidate, "owner", revision)
            .unwrap());
        let active = store.load_checkpoint(key).unwrap().unwrap();
        assert_eq!(active.cycles.len(), 1);
        assert_eq!(active.model_calls.len(), 1);
        assert_eq!(active.history.cycle_count, index - 1);
        assert_eq!(active.history.model_call_count, index - 1);
        assert_eq!(
            active.history.usage.input_tokens,
            Some((index - 1) * index / 2)
        );
        if index >= 10 {
            late_sizes.push(
                serde_json::to_vec(&checkpoint_to_value(&active, 262_144).unwrap())
                    .unwrap()
                    .len(),
            );
        }
    }
    assert!(late_sizes.iter().max().unwrap() - late_sizes.iter().min().unwrap() < 128);
    let archive = store.load_checkpoint_history(key).unwrap();
    assert_eq!(archive.cycles.len(), 47);
    assert_eq!(archive.model_calls.len(), 47);
    let active = store.load_checkpoint(key).unwrap().unwrap();
    let hydrated = vv_agent::runtime::state::hydrate_checkpoint_result(
        store,
        &active,
        terminal_result(&active, AgentStatus::Completed),
    )
    .unwrap();
    assert_eq!(hydrated.cycles.len(), 48);
    assert_eq!(hydrated.token_usage.model_calls.len(), 48);
    assert_eq!(hydrated.token_usage.input_tokens, Some(48 * 49 / 2));
    let mut forged = claim(store, key, 49);
    let revision = forged.revision;
    let mut duplicated = forged.clone();
    duplicated.cycles.push(duplicated.cycles[0].clone());
    completed_cycle(&mut duplicated, 49);
    assert!(!store
        .commit_checkpoint(duplicated, "owner", revision)
        .unwrap());
    let mut retroactive = forged.clone();
    retroactive.model_calls.push(archive.model_calls[0].clone());
    completed_cycle(&mut retroactive, 49);
    assert!(!store
        .commit_checkpoint(retroactive, "owner", revision)
        .unwrap());
    let before_reuse = store.load_checkpoint(key).unwrap().unwrap();
    let mut reused_identity = before_reuse.clone();
    let mut archived_call = archive.model_calls[0].clone();
    archived_call.cycle_index = 49;
    reused_identity.model_calls.push(archived_call);
    assert_eq!(
        store
            .progress_checkpoint(reused_identity.clone(), "owner", revision)
            .unwrap_err()
            .code(),
        "checkpoint_history_invalid"
    );
    completed_cycle(&mut reused_identity, 49);
    assert_eq!(
        store
            .commit_checkpoint(reused_identity, "owner", revision)
            .unwrap_err()
            .code(),
        "checkpoint_history_invalid"
    );
    assert_eq!(store.load_checkpoint(key).unwrap().unwrap(), before_reuse);
    assert_eq!(store.load_checkpoint_history(key).unwrap(), archive);
    let mut changed_cycle = forged.clone();
    changed_cycle.cycles[0].assistant_message = "changed historical evidence".to_string();
    assert!(!store
        .progress_checkpoint(changed_cycle, "owner", revision)
        .unwrap());
    let mut changed_call = forged.clone();
    changed_call.model_calls[0].usage.input_tokens = Some(0);
    assert!(!store
        .progress_checkpoint(changed_call, "owner", revision)
        .unwrap());
    forged.history.head_digest = Some("0".repeat(64));
    assert!(!store
        .progress_checkpoint(forged, "owner", revision)
        .unwrap());
    assert_eq!(store.load_checkpoint_history(key).unwrap(), archive);
    let mut next = store.load_checkpoint(key).unwrap().unwrap();
    completed_cycle(&mut next, 49);
    let revision = next.revision;
    assert!(store.commit_checkpoint(next, "owner", revision).unwrap());
    assert_eq!(
        vv_agent::runtime::state::hydrate_checkpoint_result(
            store,
            &active,
            terminal_result(&active, AgentStatus::Completed)
        )
        .unwrap_err()
        .code(),
        "checkpoint_history_changed"
    );
    store.delete_checkpoint(key).unwrap();
    assert!(store
        .load_checkpoint_history(key)
        .unwrap()
        .cycles
        .is_empty());
    assert!(store.create_checkpoint(minimal_checkpoint(key)).unwrap());
    for index in 1..=2 {
        let mut candidate = claim(store, key, index);
        completed_cycle(&mut candidate, index as u32);
        let revision = candidate.revision;
        assert!(store
            .commit_checkpoint(candidate, "owner", revision)
            .unwrap());
    }
    store.delete_checkpoint(key).unwrap();
}

#[test]
fn memory_history_is_bounded_and_stale_claims_do_not_append() {
    exercise_history(&InMemoryCheckpointStore::new(), "history-memory");
}

#[test]
fn sqlite_history_is_bounded_and_stale_claims_do_not_append() {
    let directory = tempdir().unwrap();
    exercise_history(
        &SqliteCheckpointStore::new(directory.path().join("history.sqlite3")).unwrap(),
        "history-sqlite",
    );
}

#[test]
fn redis_history_is_bounded_and_stale_claims_do_not_append() {
    let Ok(url) = std::env::var("VV_AGENT_TEST_REDIS_URL") else {
        return;
    };
    exercise_history(
        &RedisCheckpointStore::new(url).unwrap(),
        &format!("history-redis-{}", uuid::Uuid::new_v4()),
    );
}

#[test]
fn sqlite_history_append_failure_rolls_back_checkpoint_and_outbox() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("history-failure.sqlite3");
    let store = SqliteCheckpointStore::new(&path).unwrap();
    let key = "history-failure";
    assert!(store.create_checkpoint(minimal_checkpoint(key)).unwrap());
    let mut first = claim(&store, key, 1);
    completed_cycle(&mut first, 1);
    let revision = first.revision;
    assert!(store.commit_checkpoint(first, "owner", revision).unwrap());
    let mut second = claim(&store, key, 2);
    let before = second.clone();
    completed_cycle(&mut second, 2);
    second
        .event_outbox
        .push(EventOutboxEntry::pending("history-event", current_event("history-event")).unwrap());
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_archive BEFORE INSERT ON checkpoint_history BEGIN SELECT RAISE(ABORT, 'archive unavailable'); END;").unwrap();
    assert!(store
        .commit_checkpoint(second.clone(), "owner", before.revision)
        .is_err());
    assert_eq!(store.load_checkpoint(key).unwrap().unwrap(), before);
    assert!(store
        .load_checkpoint_history(key)
        .unwrap()
        .cycles
        .is_empty());
    connection
        .execute_batch("DROP TRIGGER reject_archive")
        .unwrap();
    connection.execute_batch("CREATE TRIGGER reject_archive_identity BEFORE INSERT ON checkpoint_history_call_ids BEGIN SELECT RAISE(ABORT, 'identity index unavailable'); END;").unwrap();
    assert!(store
        .commit_checkpoint(second.clone(), "owner", before.revision)
        .is_err());
    assert_eq!(store.load_checkpoint(key).unwrap().unwrap(), before);
    assert!(store
        .load_checkpoint_history(key)
        .unwrap()
        .cycles
        .is_empty());
    connection
        .execute_batch("DROP TRIGGER reject_archive_identity")
        .unwrap();
    assert!(store
        .commit_checkpoint(second, "owner", before.revision)
        .unwrap());
    assert_eq!(store.load_checkpoint_history(key).unwrap().cycles.len(), 1);
    connection
        .execute(
            "UPDATE checkpoint_history SET payload = replace(payload, 'xxxxxxxx', 'yyyyyyyy')",
            [],
        )
        .unwrap();
    assert_eq!(
        store.load_checkpoint_history(key).unwrap_err().code(),
        "checkpoint_history_invalid"
    );
}

#[test]
fn redis_archive_failure_does_not_commit_checkpoint_or_outbox() {
    let Ok(url) = std::env::var("VV_AGENT_TEST_REDIS_URL") else {
        return;
    };
    let store = RedisCheckpointStore::new(&url).unwrap();
    let key = format!("history-failure-{}", uuid::Uuid::new_v4());
    assert!(store.create_checkpoint(minimal_checkpoint(&key)).unwrap());
    let mut first = claim(&store, &key, 1);
    completed_cycle(&mut first, 1);
    let revision = first.revision;
    assert!(store.commit_checkpoint(first, "owner", revision).unwrap());
    let mut second = claim(&store, &key, 2);
    let before = second.clone();
    completed_cycle(&mut second, 2);
    second
        .event_outbox
        .push(EventOutboxEntry::pending("history-event", current_event("history-event")).unwrap());
    let client = redis::Client::open(url).unwrap();
    let mut connection = client.get_connection().unwrap();
    let history_key = RedisCheckpointStore::history_key(&key);
    redis::Commands::set::<_, _, ()>(&mut connection, &history_key, "invalid-history-type")
        .unwrap();
    assert!(store
        .commit_checkpoint(second.clone(), "owner", before.revision)
        .is_err());
    assert_eq!(store.load_checkpoint(&key).unwrap().unwrap(), before);
    redis::Commands::del::<_, ()>(&mut connection, history_key).unwrap();
    let identity_key = RedisCheckpointStore::history_call_ids_key(&key);
    redis::Commands::set::<_, _, ()>(&mut connection, &identity_key, "invalid-index-type").unwrap();
    assert!(store
        .commit_checkpoint(second.clone(), "owner", before.revision)
        .is_err());
    assert_eq!(store.load_checkpoint(&key).unwrap().unwrap(), before);
    assert!(store
        .load_checkpoint_history(&key)
        .unwrap()
        .cycles
        .is_empty());
    redis::Commands::del::<_, ()>(&mut connection, identity_key).unwrap();
    assert!(store
        .commit_checkpoint(second, "owner", before.revision)
        .unwrap());
    assert_eq!(store.load_checkpoint_history(&key).unwrap().cycles.len(), 1);
    store.delete_checkpoint(&key).unwrap();
}

#[test]
fn history_frontier_rejects_missing_nullable_fields() {
    let checkpoint = minimal_checkpoint("history-strict");
    for pointer in [
        "/history/head_digest",
        "/history/previous_agent_input",
        "/history/usage/input_tokens",
        "/history/usage/cache_usage",
    ] {
        let mut payload = checkpoint_to_value(&checkpoint, 262_144).unwrap();
        remove_pointer(&mut payload, pointer);
        assert_eq!(
            checkpoint_from_value(&payload, 262_144).unwrap_err().code(),
            "checkpoint_history_invalid"
        );
    }
}

#[test]
#[ignore = "requires paired cross-runtime history store"]
fn cross_runtime_history_store() {
    let location = std::env::var("VV_AGENT_CROSS_HISTORY_LOCATION")
        .expect("VV_AGENT_CROSS_HISTORY_LOCATION is required for paired history tests");
    let store: Box<dyn CheckpointStore> = match std::env::var("VV_AGENT_CROSS_HISTORY_STORE")
        .unwrap()
        .as_str()
    {
        "sqlite" => Box::new(SqliteCheckpointStore::new(location).unwrap()),
        "redis" => Box::new(RedisCheckpointStore::new(location).unwrap()),
        other => panic!("unsupported history test store {other}"),
    };
    let key = "cross-history";
    match std::env::var("VV_AGENT_CROSS_HISTORY_MODE")
        .unwrap()
        .as_str()
    {
        "write" => {
            store.delete_checkpoint(key).unwrap();
            assert!(store.create_checkpoint(minimal_checkpoint(key)).unwrap());
            for index in 1..=3 {
                let mut candidate = claim(store.as_ref(), key, index);
                completed_cycle(&mut candidate, index as u32);
                let revision = candidate.revision;
                assert!(store
                    .commit_checkpoint(candidate, "owner", revision)
                    .unwrap());
            }
        }
        "read" => {
            let checkpoint = store.load_checkpoint(key).unwrap().unwrap();
            assert_eq!(checkpoint.history.cycle_count, 2);
            assert_eq!(checkpoint.history.model_call_count, 2);
            let result = vv_agent::runtime::state::hydrate_checkpoint_result(
                store.as_ref(),
                &checkpoint,
                terminal_result(&checkpoint, AgentStatus::Completed),
            )
            .unwrap();
            assert_eq!(
                result
                    .cycles
                    .iter()
                    .map(|cycle| cycle.index)
                    .collect::<Vec<_>>(),
                vec![1, 2, 3]
            );
            assert_eq!(result.token_usage.input_tokens, Some(6));
            let mut candidate = claim(store.as_ref(), key, 4);
            completed_cycle(&mut candidate, 4);
            let revision = candidate.revision;
            let mut reused_identity = candidate.clone();
            reused_identity.model_calls.last_mut().unwrap().call_id = "call-1".to_string();
            assert_eq!(
                store
                    .commit_checkpoint(reused_identity, "owner", revision)
                    .unwrap_err()
                    .code(),
                "checkpoint_history_invalid"
            );
            assert!(store
                .commit_checkpoint(candidate, "owner", revision)
                .unwrap());
            let checkpoint = store.load_checkpoint(key).unwrap().unwrap();
            assert_eq!(checkpoint.history.cycle_count, 3);
            assert_eq!(store.load_checkpoint_history(key).unwrap().cycles.len(), 3);
        }
        other => panic!("unsupported history test mode {other}"),
    }
}

#[test]
fn canonical_history_batch_and_frontier_match_real_compaction() {
    let case = fixture(CODEC_FIXTURE)["history_archive_cases"]["one_committed_cycle"].clone();
    let mut checkpoint = minimal_checkpoint("fresh");
    checkpoint.cycles = case["batch"]["cycles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| CycleRecord::from_dict(value).unwrap())
        .collect();
    checkpoint.model_calls = serde_json::from_value(case["batch"]["model_calls"].clone()).unwrap();
    checkpoint.cycles.push(CycleRecord {
        index: 2,
        assistant_message: "second".to_string(),
        tool_calls: Vec::new(),
        tool_results: Vec::new(),
        memory_compacted: false,
    });
    checkpoint.cycle_index = 2;
    let batch = vv_agent::runtime::state::normalize_checkpoint_history(&mut checkpoint)
        .unwrap()
        .unwrap();
    assert_eq!(batch.payload, case["batch"]);
    assert_eq!(
        serde_json::to_value(&checkpoint.history).unwrap(),
        case["frontier"]
    );
    let archived = vv_agent::runtime::state::decode_checkpoint_history(
        &checkpoint,
        &[serde_json::to_string(&batch.payload).unwrap()],
    )
    .unwrap();
    assert_eq!(archived.cycles.len(), 1);
    let raw = serde_json::to_string(&batch.payload).unwrap();
    let duplicate = format!("{{\"schema_version\":\"ignored\",{}", &raw[1..]);
    assert!(
        vv_agent::runtime::state::decode_checkpoint_history(&checkpoint, &[duplicate])
            .unwrap_err()
            .message()
            .contains("duplicate")
    );
    checkpoint.cycles.insert(0, archived.cycles[0].clone());
    assert_eq!(
        vv_agent::runtime::state::decode_checkpoint_history(
            &checkpoint,
            &[serde_json::to_string(&batch.payload).unwrap()]
        )
        .unwrap_err()
        .code(),
        "checkpoint_history_invalid"
    );
}
