use super::*;

#[test]
fn redis_store_admits_and_recovers_with_durable_replay() {
    let Ok(redis_url) = std::env::var("VV_AGENT_TEST_REDIS_URL") else {
        return;
    };
    let store = RedisCheckpointStore::new(&redis_url).expect("redis");
    let keep_fixture = std::env::var_os("VV_AGENT_KEEP_REDIS_FIXTURE").is_some();
    for checkpoint_key in store
        .list_checkpoints()
        .expect("list test checkpoints")
        .into_iter()
    {
        store
            .delete_checkpoint(&checkpoint_key)
            .expect("clean test checkpoint");
    }
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let key = format!(
        "checkpoint-controller-redis-{}-{suffix}",
        std::process::id()
    );
    let mut checkpoint = minimal_checkpoint();
    checkpoint.checkpoint_key = key.clone();
    store
        .delete_checkpoint(&key)
        .expect("clean stale checkpoint");
    assert!(store.create_checkpoint(checkpoint).expect("create"));
    let claimed = store
        .claim_checkpoint(&key, 1, "redis-worker", 1_000_000, 0, ClaimMode::Continue)
        .expect("claim")
        .expect("claimed");
    let request = HostInteractionRequest::new(
        format!("redis-interaction-{suffix}"),
        1,
        format!("redis-operation-{suffix}"),
        format!("redis-tool-{suffix}"),
        "Pick one.",
    )
    .expect("request");
    let expired_context = HostInteractionAdmissionContext::new(
        &key,
        claimed.revision,
        "redis-worker",
        1,
        1_000_000,
        1_000_000,
    )
    .expect("expired context shape");
    let error = store
        .produce_host_interaction(request.clone(), &expired_context)
        .err()
        .expect("expired redis claim");
    assert_eq!(error.code(), "host_interaction_claim_required");
    let admission = HostInteractionAdmissionContext::new(
        &key,
        claimed.revision,
        "redis-worker",
        claimed.claimed_cycle.expect("claimed cycle"),
        0,
        claimed.lease_expires_at_ms.expect("claim lease"),
    )
    .expect("admission context");
    let admitted = store
        .produce_host_interaction(request.clone(), &admission)
        .expect("produce");
    assert_eq!(admitted.status, "admitted");
    assert_eq!(
        store
            .produce_host_interaction(request.clone(), &admission,)
            .expect("replay")
            .status,
        "replayed"
    );
    let command = ControllerCommand::new(
        format!("redis-command-{suffix}"),
        ControllerHandle::new(&key, &claimed.root_run_id, &claimed.trace_id).expect("handle"),
        1,
        admitted.checkpoint_revision,
        ControllerCommandVariant::HostInteractionResponse {
            interaction_id: request.interaction_id.clone(),
            logical_cycle: 1,
            operation_id: request.operation_id.clone(),
            tool_call_id: request.tool_call_id.clone(),
            request_digest: request.request_digest.clone(),
            response: HostInteractionMessage::user("redis-approved").expect("response"),
        },
    )
    .expect("command");
    let resolution = store
        .resolve_controller_command(command.clone())
        .expect("resolve");
    let wake_cycle = match &resolution {
        ControllerCommandResolution::Applied { wake, .. } => wake.logical_cycle,
        other => panic!("unexpected resolution: {other:?}"),
    };
    assert_eq!(wake_cycle, 1);
    let client = redis::Client::open(redis_url.as_str()).expect("redis client");
    let mut connection = client.get_connection().expect("redis connection");
    let receipt_key = RedisCheckpointStore::controller_command_key(&command.command_id);
    let command_key = RedisCheckpointStore::controller_command_payload_key(&command.command_id);
    let receipt_wire: Value = serde_json::from_str(
        &connection
            .get::<_, String>(&receipt_key)
            .expect("receipt wire"),
    )
    .expect("receipt JSON");
    assert_eq!(
        receipt_wire["schema_version"],
        "vv-agent.controller-command-receipt.v1"
    );
    assert!(receipt_wire.get("command").is_none());
    let command_wire: Value = serde_json::from_str(
        &connection
            .get::<_, String>(&command_key)
            .expect("command payload wire"),
    )
    .expect("command JSON");
    assert_eq!(
        command_wire["schema_version"],
        "vv-agent.controller-command.v1"
    );
    assert!(command_wire.get("resolution").is_none());
    let record_key = RedisCheckpointStore::host_interaction_key(&key, &request.interaction_id);
    let expected_record_key = format!(
        "vv-agent:host-interaction:{:x}",
        Sha256::digest(format!("{key}\0{}", request.interaction_id).as_bytes())
    );
    assert_eq!(record_key, expected_record_key);
    let record_wire: Value = serde_json::from_str(
        &connection
            .get::<_, String>(&record_key)
            .expect("host record wire"),
    )
    .expect("host record JSON");
    assert_eq!(
        record_wire["schema_version"],
        "vv-agent.host-interaction-record.v1"
    );
    assert_eq!(
        record_wire["request"]["request_digest"],
        request.request_digest
    );
    let notification_key =
        RedisCheckpointStore::host_interaction_notification_key(&admitted.notification_id);
    let expected_notification_key = format!(
        "vv-agent:host-interaction-notification:{:x}",
        Sha256::digest(admitted.notification_id.as_bytes())
    );
    assert_eq!(notification_key, expected_notification_key);
    let notification_wire: Value = serde_json::from_str(
        &connection
            .get::<_, String>(&notification_key)
            .expect("notification wire"),
    )
    .expect("notification JSON");
    assert_eq!(
        notification_wire["notification_id"],
        admitted.notification_id
    );
    assert_eq!(
        notification_wire["payload_digest"],
        admitted.notification_payload_digest
    );
    assert_eq!(
        notification_wire
            .as_object()
            .expect("notification object")
            .len(),
        13
    );
    let notification_claim = store
        .claim_host_interaction_notification(
            &admitted.notification_id,
            &admitted.notification_payload_digest,
            "redis-notification-owner",
            10_000,
            1,
        )
        .expect("claim notification")
        .expect("notification row");
    let ambiguous_notification = store
        .complete_host_interaction_notification(
            &admitted.notification_id,
            &admitted.notification_payload_digest,
            notification_claim
                .claim_token
                .as_deref()
                .expect("notification claim token"),
            notification_claim.attempt,
            "ambiguous",
            2,
            Some("observer callback was interrupted"),
        )
        .expect("mark notification ambiguous")
        .expect("ambiguous notification");
    let delivered_notification = store
        .reconcile_host_interaction_notification(
            &admitted.notification_id,
            &admitted.notification_payload_digest,
            "delivered",
            3,
            None,
        )
        .expect("reconcile notification")
        .expect("delivered notification");
    assert_eq!(
        delivered_notification.outbox_state,
        vv_agent::NotificationOutboxState::Delivered
    );
    assert_eq!(
        store
            .reconcile_host_interaction_notification(
                &admitted.notification_id,
                &admitted.notification_payload_digest,
                "delivered",
                4,
                None,
            )
            .expect("same notification replay")
            .expect("replayed notification")
            .outbox_state,
        vv_agent::NotificationOutboxState::Delivered
    );
    assert!(store
        .reconcile_host_interaction_notification(
            &admitted.notification_id,
            &"0".repeat(64),
            "delivered",
            5,
            None,
        )
        .is_err());
    assert_eq!(
        ambiguous_notification.outbox_state,
        vv_agent::NotificationOutboxState::Ambiguous
    );
    assert_eq!(
        store
            .resolve_controller_command(command.clone())
            .expect("command replay")
            .kind(),
        "replayed"
    );
    let claimed_wake = store
        .claim_controller_command_wake(
            &command.command_id,
            &command.command_digest,
            "redis-wake-owner-a",
            10_000,
            1,
        )
        .expect("claim wake")
        .expect("wake receipt");
    assert_eq!(claimed_wake.outbox_state, "claimed");
    assert_eq!(claimed_wake.outbox_attempt, 1);
    assert!(store
        .complete_controller_command_wake(
            &command.command_id,
            &command.command_digest,
            "redis-stale-owner",
            1,
            "delivered",
            2,
            None,
        )
        .is_err());
    let ambiguous = store
        .complete_controller_command_wake(
            &command.command_id,
            &command.command_digest,
            "redis-wake-owner-a",
            1,
            "ambiguous",
            2,
            Some("callback https://provider.test/?token=secret"),
        )
        .expect("ambiguous wake")
        .expect("ambiguous receipt");
    assert_eq!(ambiguous.outbox_state, "ambiguous");
    let retried = store
        .reconcile_controller_command_wake(&command.command_id, &command.command_digest, "retry", 3)
        .expect("reconcile wake")
        .expect("retried receipt");
    assert_eq!(retried.outbox_state, "pending");
    let envelope = HostInteractionRecoveryEnvelope {
        schema_version: "vv-agent.host-interaction-recovery.v1".to_string(),
        record_id: admitted.record_id.clone(),
        checkpoint_key: key.clone(),
        run_id: claimed.root_run_id,
        trace_id: claimed.trace_id,
        claim_mode: "recovery".to_string(),
        resume_attempt: 1,
        expected_revision: admitted.checkpoint_revision + 1,
        logical_cycle: 1,
        interaction_id: request.interaction_id,
        operation_id: request.operation_id,
        tool_call_id: request.tool_call_id,
        request_digest: request.request_digest,
        command_id: command.command_id,
    };
    assert_eq!(
        store
            .claim_and_consume_host_interaction_response(envelope.clone())
            .expect("recover")
            .kind,
        "applied"
    );
    assert_eq!(
        store
            .claim_and_consume_host_interaction_response(envelope)
            .expect("recover replay")
            .kind,
        "replayed"
    );
    if !keep_fixture {
        store.delete_checkpoint(&key).expect("cleanup");
    } else {
        eprintln!("kept Redis parity fixture for cross-language probe: {key}");
    }
}
