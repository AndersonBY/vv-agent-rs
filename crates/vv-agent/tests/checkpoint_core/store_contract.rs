use super::*;

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
fn redis_store_rejects_non_initial_create_without_writes() {
    let Ok(redis_url) = std::env::var("VV_AGENT_TEST_REDIS_URL") else {
        return;
    };
    let store = RedisCheckpointStore::new(&redis_url).expect("redis");
    let suffix = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let key = format!(
        "checkpoint-create-rejection-{}-{suffix}",
        std::process::id()
    );
    store
        .delete_checkpoint(&key)
        .expect("clean stale checkpoint");

    let mut checkpoint = minimal_checkpoint(&key);
    checkpoint.revision = 1;
    let error = store
        .create_checkpoint(checkpoint)
        .expect_err("Redis must reject non-initial create");
    assert_eq!(error.code(), "checkpoint_initial_invalid");

    let client = redis::Client::open(redis_url).expect("redis client");
    let mut connection = client.get_connection().expect("redis connection");
    let data_key = RedisCheckpointStore::data_key(&key);
    let lease_key = RedisCheckpointStore::lease_key(&key);
    let data: Option<String> = redis::Commands::get(&mut connection, &data_key).unwrap();
    let lease: Option<u64> = redis::Commands::get(&mut connection, &lease_key).unwrap();
    let indexed: bool =
        redis::Commands::sismember(&mut connection, "vv-agent:checkpoint-keys", &key).unwrap();
    assert!(data.is_none());
    assert!(lease.is_none());
    assert!(!indexed);
}

#[test]
#[ignore = "requires VV_AGENT_REDIS_URL and a live Redis instance"]
fn redis_store_supports_claimed_finalize_and_event_delivery() {
    let redis_url = std::env::var("VV_AGENT_REDIS_URL").expect("VV_AGENT_REDIS_URL");
    let store = RedisCheckpointStore::new(redis_url).unwrap();
    let prefix = format!("redis-current-{}", uuid::Uuid::new_v4());
    exercise_current_store_contract(&store, &prefix);
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
