use super::*;

#[path = "redis_host_binding.rs"]
mod redis_host_binding;

#[test]
fn redis_operation_error_does_not_poison_the_next_transaction() {
    let Ok(redis_url) = std::env::var("VV_AGENT_TEST_REDIS_URL") else {
        return;
    };
    let store = RedisCheckpointStore::new(&redis_url).expect("redis");
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();

    let failed_key = format!("checkpoint-redis-unwatch-failed-{suffix}");
    let mut failed_checkpoint = minimal_checkpoint();
    failed_checkpoint.checkpoint_key = failed_key.clone();
    assert!(store.create_checkpoint(failed_checkpoint).expect("create"));
    let current = store
        .load_checkpoint(&failed_key)
        .expect("load")
        .expect("checkpoint");
    let command = ControllerCommand::new(
        format!("redis-unwatch-command-{suffix}"),
        ControllerHandle::new(&failed_key, &current.root_run_id, &current.trace_id)
            .expect("handle"),
        current.resume_attempt,
        current.revision,
        ControllerCommandVariant::Resume,
    )
    .expect("resume command");
    let error = store
        .resolve_controller_command(command)
        .expect_err("invalid resume should fail inside the Redis transaction");
    assert_eq!(error.code(), "controller_command_invalid_state");

    let client = redis::Client::open(redis_url.as_str()).expect("redis client");
    let mut connection = client.get_connection().expect("redis connection");
    let data_key = RedisCheckpointStore::data_key(&failed_key);
    let original_payload: String = connection.get(&data_key).expect("checkpoint payload");
    connection
        .set::<_, _, ()>(&data_key, "changed")
        .expect("mutate watched key");

    let unrelated_key = format!("checkpoint-redis-unwatch-next-{suffix}");
    let mut unrelated_checkpoint = minimal_checkpoint();
    unrelated_checkpoint.checkpoint_key = unrelated_key.clone();
    assert!(store
        .create_checkpoint(unrelated_checkpoint)
        .expect("unrelated transaction after error"));
    connection
        .set::<_, _, ()>(&data_key, original_payload)
        .expect("restore failed checkpoint payload");
    store
        .delete_checkpoint(&failed_key)
        .expect("cleanup failed checkpoint");
    store
        .delete_checkpoint(&unrelated_key)
        .expect("cleanup unrelated checkpoint");
}

#[test]
fn redis_expired_claim_cancel_uses_unknown_outcome_error_code() {
    let Ok(redis_url) = std::env::var("VV_AGENT_TEST_REDIS_URL") else {
        return;
    };
    let store = RedisCheckpointStore::new(&redis_url).expect("redis");
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let key = format!("checkpoint-redis-expired-cancel-{suffix}");
    store
        .delete_checkpoint(&key)
        .expect("clean stale checkpoint");

    let mut checkpoint = minimal_checkpoint();
    checkpoint.checkpoint_key = key.clone();
    assert!(store.create_checkpoint(checkpoint).expect("create"));
    let claimed = store
        .claim_checkpoint(&key, 1, "expired-cancel-owner", 1, 0, ClaimMode::Continue)
        .expect("claim")
        .expect("claimed checkpoint");
    let command = ControllerCommand::new(
        format!("redis-expired-cancel-command-{suffix}"),
        ControllerHandle::new(&key, &claimed.root_run_id, &claimed.trace_id).expect("handle"),
        claimed.resume_attempt,
        claimed.revision,
        ControllerCommandVariant::Cancel,
    )
    .expect("cancel command");
    store
        .resolve_controller_command(command)
        .expect("expired cancellation");

    let terminal = store
        .load_checkpoint(&key)
        .expect("load terminal")
        .expect("terminal checkpoint");
    assert_eq!(terminal.status, vv_agent::CheckpointStatus::Failed);
    assert_eq!(
        terminal.terminal_result.as_ref().unwrap()["error"]["code"],
        "cancelled_with_unknown_outcome"
    );
    assert_eq!(
        terminal.terminal_result.as_ref().unwrap()["completion_reason"],
        "cancelled"
    );
    assert!(terminal.event_outbox.iter().any(|entry| {
        entry.event["type"] == "run_cancelled" && entry.event["reason"] == "Operation was cancelled"
    }));
    store.delete_checkpoint(&key).expect("cleanup");
}

#[test]
fn redis_delete_error_does_not_poison_the_next_transaction() {
    let Ok(redis_url) = std::env::var("VV_AGENT_TEST_REDIS_URL") else {
        return;
    };
    let store = RedisCheckpointStore::new(&redis_url).expect("redis");
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();

    let failed_key = format!("checkpoint-redis-delete-unwatch-failed-{suffix}");
    let mut failed_checkpoint = minimal_checkpoint();
    failed_checkpoint.checkpoint_key = failed_key.clone();
    assert!(store.create_checkpoint(failed_checkpoint).expect("create"));

    let client = redis::Client::open(redis_url.as_str()).expect("redis client");
    let mut connection = client.get_connection().expect("redis connection");
    let receipt_set_key = RedisCheckpointStore::deferred_receipts_checkpoint_set_key(&failed_key);
    connection
        .set::<_, _, ()>(&receipt_set_key, "wrong-type")
        .expect("install delete callback error");
    let error = store
        .delete_checkpoint(&failed_key)
        .expect_err("wrong receipt index type should fail delete");
    assert_eq!(error.code(), "checkpoint_store_redis");

    let data_key = RedisCheckpointStore::data_key(&failed_key);
    let original_payload: String = connection.get(&data_key).expect("checkpoint payload");
    connection
        .set::<_, _, ()>(&data_key, "changed")
        .expect("mutate watched key after delete error");

    let unrelated_key = format!("checkpoint-redis-delete-unwatch-next-{suffix}");
    let mut unrelated_checkpoint = minimal_checkpoint();
    unrelated_checkpoint.checkpoint_key = unrelated_key.clone();
    assert!(store
        .create_checkpoint(unrelated_checkpoint)
        .expect("unrelated transaction after delete error"));

    connection
        .del::<_, ()>(&receipt_set_key)
        .expect("remove wrong-type index");
    connection
        .set::<_, _, ()>(&data_key, original_payload)
        .expect("restore failed checkpoint payload");
    store
        .delete_checkpoint(&failed_key)
        .expect("cleanup failed checkpoint");
    store
        .delete_checkpoint(&unrelated_key)
        .expect("cleanup unrelated checkpoint");
}

#[test]
#[ignore = "requires a Python-seeded cross-language fixture and is run as an explicit probe"]
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
