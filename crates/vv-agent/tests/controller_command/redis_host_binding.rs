use super::*;

#[test]
fn redis_checkpoint_payload_key_binding_is_fail_closed() {
    let Ok(redis_url) = std::env::var("VV_AGENT_TEST_REDIS_URL") else {
        return;
    };
    let store = RedisCheckpointStore::new(&redis_url).expect("redis");
    let key = format!(
        "checkpoint-redis-payload-binding-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    );
    let mut checkpoint = minimal_checkpoint();
    checkpoint.checkpoint_key = key.clone();
    store.delete_checkpoint(&key).expect("clean checkpoint");
    assert!(store.create_checkpoint(checkpoint).expect("create"));
    let data_key = RedisCheckpointStore::data_key(&key);
    let client = redis::Client::open(redis_url.as_str()).expect("redis client");
    let mut connection = client.get_connection().expect("redis connection");
    let original: String = connection.get(&data_key).expect("checkpoint payload");
    let mut tampered: Value = serde_json::from_str(&original).expect("payload JSON");
    tampered["checkpoint_key"] = json!("foreign-checkpoint");
    connection
        .set::<_, _, ()>(
            &data_key,
            serde_json::to_string(&tampered).expect("tampered JSON"),
        )
        .expect("tamper payload");
    let error = store
        .load_checkpoint(&key)
        .expect_err("foreign checkpoint payload");
    assert_eq!(error.code(), "checkpoint_store_conflict");
    assert_eq!(
        store
            .list_checkpoints()
            .expect_err("list must reject a payload bound to another key")
            .code(),
        "checkpoint_store_conflict"
    );
    assert_eq!(
        store
            .delete_checkpoint(&key)
            .expect_err("delete must reject a payload bound to another key")
            .code(),
        "checkpoint_store_conflict"
    );
    connection
        .set::<_, _, ()>(&data_key, original)
        .expect("restore payload");
    store.delete_checkpoint(&key).expect("cleanup");
}

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
    let reaped = store
        .reap_controller_command_wakes(&key, 1)
        .expect("reap pending redis wake");
    assert_eq!(reaped.len(), 1);
    assert_eq!(reaped[0].command_id, command.command_id);
    assert_eq!(reaped[0].checkpoint_key, key);
    assert_eq!(reaped[0].handle, command.handle);
    assert_eq!(
        reaped[0].outbox_id,
        vv_agent::checkpoint::controller_receipt_outbox_id(
            &command.command_id,
            &command.command_digest,
        )
        .expect("outbox id")
    );
    assert_eq!(reaped[0].outbox_state, "pending");
    assert_eq!(reaped[0].outbox_action, "recovery_dispatch");
    assert_eq!(
        reaped[0].outbox_destination.as_deref(),
        Some("distributed_advance")
    );
    assert_eq!(reaped[0].attempt, 0);
    assert!(reaped[0].claim_token.is_none());
    assert!(reaped[0].lease_expires_at_ms.is_none());
    let before_barrier = store
        .load_checkpoint(&key)
        .expect("load before redis recovery barrier")
        .expect("redis checkpoint before recovery barrier");
    for (claim_mode, claim_token) in [
        (ClaimMode::Continue, "redis-ordinary-continue"),
        (ClaimMode::Recovery, "redis-ordinary-recovery"),
    ] {
        let error = store
            .claim_checkpoint(&key, 2, claim_token, 1_000_000, 0, claim_mode)
            .expect_err("ordinary redis claim must stop at host recovery barrier");
        assert_eq!(error.code(), "host_interaction_recovery_required");
        let after_barrier = store
            .load_checkpoint(&key)
            .expect("load after rejected redis claim")
            .expect("redis checkpoint after rejected claim");
        assert_eq!(after_barrier, before_barrier);
    }
    let client = redis::Client::open(redis_url.as_str()).expect("redis client");
    let mut connection = client.get_connection().expect("redis connection");
    let foreign_key = format!("checkpoint-redis-binding-foreign-{suffix}");
    store
        .delete_checkpoint(&foreign_key)
        .expect("clean foreign checkpoint");
    let mut foreign_checkpoint = minimal_checkpoint();
    foreign_checkpoint.checkpoint_key = foreign_key.clone();
    assert!(store
        .create_checkpoint(foreign_checkpoint)
        .expect("create foreign checkpoint"));
    let foreign_claimed = store
        .claim_checkpoint(
            &foreign_key,
            1,
            "redis-foreign-worker",
            1_000_000,
            0,
            ClaimMode::Continue,
        )
        .expect("claim foreign checkpoint")
        .expect("foreign checkpoint claim");
    let foreign_context = HostInteractionAdmissionContext::new(
        &foreign_key,
        foreign_claimed.revision,
        "redis-foreign-worker",
        foreign_claimed
            .claimed_cycle
            .expect("foreign claimed cycle"),
        0,
        foreign_claimed.lease_expires_at_ms.expect("foreign lease"),
    )
    .expect("foreign admission context");
    let foreign_admitted = store
        .produce_host_interaction(request.clone(), &foreign_context)
        .expect("produce foreign interaction");
    let replay_record_key =
        RedisCheckpointStore::host_interaction_key(&key, &request.interaction_id);
    let replay_notification_key =
        RedisCheckpointStore::host_interaction_notification_key(&admitted.notification_id);
    let data_key = RedisCheckpointStore::data_key(&key);
    let original_checkpoint_wire: String = connection.get(&data_key).expect("checkpoint wire");
    let original_record_wire: String = connection
        .get(&replay_record_key)
        .expect("original record wire");
    let original_notification_wire: String = connection
        .get(&replay_notification_key)
        .expect("original notification wire");
    let foreign_record_key =
        RedisCheckpointStore::host_interaction_key(&foreign_key, &request.interaction_id);
    let foreign_notification_key =
        RedisCheckpointStore::host_interaction_notification_key(&foreign_admitted.notification_id);
    let foreign_record_wire: String = connection
        .get(&foreign_record_key)
        .expect("foreign record wire");
    let foreign_notification_wire: String = connection
        .get(&foreign_notification_key)
        .expect("foreign notification wire");
    connection
        .set::<_, _, ()>(&replay_record_key, &foreign_record_wire)
        .expect("place foreign record under expected key");
    let error = store
        .produce_host_interaction(request.clone(), &admission)
        .err()
        .expect("replay must reject a foreign record payload");
    assert_eq!(error.code(), "host_interaction_conflict");
    assert_eq!(
        connection
            .get::<_, String>(&data_key)
            .expect("checkpoint remains unchanged"),
        original_checkpoint_wire
    );
    assert_eq!(
        connection
            .get::<_, String>(&replay_notification_key)
            .expect("notification remains unchanged"),
        original_notification_wire
    );
    connection
        .set::<_, _, ()>(&replay_record_key, &original_record_wire)
        .expect("restore original record");
    connection
        .set::<_, _, ()>(&replay_notification_key, &foreign_notification_wire)
        .expect("place foreign notification under expected key");
    let error = store
        .produce_host_interaction(request.clone(), &admission)
        .err()
        .expect("replay must reject a foreign notification payload");
    assert_eq!(error.code(), "host_interaction_conflict");
    assert_eq!(
        connection
            .get::<_, String>(&data_key)
            .expect("checkpoint remains unchanged"),
        original_checkpoint_wire
    );
    assert_eq!(
        connection
            .get::<_, String>(&replay_record_key)
            .expect("record remains unchanged"),
        original_record_wire
    );
    connection
        .set::<_, _, ()>(&replay_notification_key, &original_notification_wire)
        .expect("restore original notification");
    let dispatch_checkpoint = store
        .load_checkpoint(&key)
        .expect("load checkpoint for dispatch binding probe")
        .expect("dispatch checkpoint");
    let dispatch_probe = ControllerCommand::new(
        format!("redis-dispatch-binding-{suffix}"),
        ControllerHandle::new(
            &key,
            &dispatch_checkpoint.root_run_id,
            &dispatch_checkpoint.trace_id,
        )
        .expect("dispatch handle"),
        dispatch_checkpoint.resume_attempt,
        dispatch_checkpoint.revision,
        ControllerCommandVariant::HostInteractionResponse {
            interaction_id: request.interaction_id.clone(),
            logical_cycle: request.logical_cycle,
            operation_id: request.operation_id.clone(),
            tool_call_id: request.tool_call_id.clone(),
            request_digest: request.request_digest.clone(),
            response: HostInteractionMessage::user("dispatch-probe").expect("response"),
        },
    )
    .expect("dispatch probe command");
    connection
        .set::<_, _, ()>(&replay_record_key, &foreign_record_wire)
        .expect("place foreign record for controller dispatch");
    let error = store
        .resolve_controller_command(dispatch_probe)
        .expect_err("controller dispatch must reject a foreign record payload");
    assert_eq!(error.code(), "host_interaction_conflict");
    assert_eq!(
        connection
            .get::<_, String>(&data_key)
            .expect("checkpoint remains unchanged"),
        original_checkpoint_wire
    );
    connection
        .set::<_, _, ()>(&replay_record_key, &original_record_wire)
        .expect("restore original record after dispatch probe");
    store
        .delete_checkpoint(&foreign_key)
        .expect("cleanup foreign checkpoint");
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
    let mut tampered_receipt = receipt_wire.clone();
    tampered_receipt["handle"]["checkpoint_key"] = json!("foreign-checkpoint");
    connection
        .set::<_, _, ()>(
            &receipt_key,
            serde_json::to_string(&tampered_receipt).expect("tampered receipt"),
        )
        .expect("tamper receipt");
    assert!(store
        .get_controller_command_receipt(&command.command_id)
        .expect_err("receipt/command handle mismatch")
        .code()
        .contains("conflict"));
    connection
        .set::<_, _, ()>(
            &receipt_key,
            serde_json::to_string(&receipt_wire).expect("receipt"),
        )
        .expect("restore receipt");
    let foreign_command_id = format!("{}-foreign-id", command.command_id);
    let foreign_receipt_key = RedisCheckpointStore::controller_command_key(&foreign_command_id);
    connection
        .set::<_, _, ()>(
            &foreign_receipt_key,
            serde_json::to_string(&receipt_wire).expect("receipt copy"),
        )
        .expect("copy receipt under foreign key");
    assert!(store
        .get_controller_command_receipt(&foreign_command_id)
        .expect_err("receipt key and decoded command id must be bound")
        .code()
        .contains("conflict"));
    connection
        .del::<_, ()>(&foreign_receipt_key)
        .expect("remove foreign receipt key");
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
    let claim_probe_key = format!("checkpoint-redis-claim-binding-{suffix}");
    store
        .delete_checkpoint(&claim_probe_key)
        .expect("clean claim binding probe");
    let mut claim_probe_checkpoint = minimal_checkpoint();
    claim_probe_checkpoint.checkpoint_key = claim_probe_key.clone();
    assert!(store
        .create_checkpoint(claim_probe_checkpoint)
        .expect("create claim binding probe"));
    let claim_probe_set =
        RedisCheckpointStore::host_interactions_checkpoint_set_key(&claim_probe_key);
    let fake_member = format!("{record_key}:claim-foreign-member");
    connection
        .set::<_, _, ()>(
            &fake_member,
            serde_json::to_string(&record_wire).expect("foreign record payload"),
        )
        .expect("write claim foreign member");
    connection
        .sadd::<_, _, ()>(&claim_probe_set, &fake_member)
        .expect("index claim foreign member");
    let claim_probe_before = store
        .load_checkpoint(&claim_probe_key)
        .expect("load claim binding probe")
        .expect("claim binding probe checkpoint");
    let error = store
        .claim_checkpoint(
            &claim_probe_key,
            1,
            "claim-binding-probe",
            1_000_000,
            0,
            ClaimMode::Continue,
        )
        .expect_err("claim must reject a foreign reverse-index member");
    assert_eq!(error.code(), "host_interaction_conflict");
    assert_eq!(
        store
            .load_checkpoint(&claim_probe_key)
            .expect("reload claim binding probe")
            .expect("claim binding probe checkpoint"),
        claim_probe_before
    );
    connection
        .srem::<_, _, ()>(&claim_probe_set, &fake_member)
        .expect("remove claim foreign member");
    connection
        .del::<_, ()>(&fake_member)
        .expect("remove claim foreign record");
    store
        .delete_checkpoint(&claim_probe_key)
        .expect("cleanup claim binding probe");
    let host_set_key = RedisCheckpointStore::host_interactions_checkpoint_set_key(&key);
    let foreign_member = format!("{record_key}:foreign-member");
    connection
        .set::<_, _, ()>(
            &foreign_member,
            serde_json::to_string(&record_wire).expect("record"),
        )
        .expect("foreign record");
    connection
        .sadd::<_, _, ()>(&host_set_key, &foreign_member)
        .expect("foreign index member");
    assert!(store
        .find_resolved_pending_host_interaction(&key)
        .expect_err("foreign host reverse-index member")
        .code()
        .contains("conflict"));
    assert!(store
        .reap_host_interaction_record(&admitted.record_id, &key, 2)
        .expect_err("foreign host reverse-index member")
        .code()
        .contains("conflict"));
    assert!(store
        .delete_checkpoint(&key)
        .expect_err("foreign reverse-index member")
        .code()
        .contains("conflict"));
    assert!(store
        .load_checkpoint(&key)
        .expect("checkpoint remains readable")
        .is_some());
    connection
        .srem::<_, _, ()>(&host_set_key, &foreign_member)
        .expect("remove foreign member");
    connection
        .del::<_, ()>(&foreign_member)
        .expect("remove record");
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
    let foreign_notification_id = format!("{}-foreign-id", admitted.notification_id);
    let foreign_notification_key =
        RedisCheckpointStore::host_interaction_notification_key(&foreign_notification_id);
    let original_notification = serde_json::to_string(&notification_wire).expect("notification");
    connection
        .set::<_, _, ()>(&foreign_notification_key, &original_notification)
        .expect("copy notification under foreign key");
    assert!(store
        .get_host_interaction_notification(&foreign_notification_id)
        .expect_err("notification key and decoded id must be bound")
        .code()
        .contains("conflict"));
    assert!(store
        .claim_host_interaction_notification(
            &foreign_notification_id,
            &admitted.notification_payload_digest,
            "foreign-notification-owner",
            10_000,
            1,
        )
        .expect_err("notification claim must reject foreign storage key")
        .code()
        .contains("conflict"));
    assert!(store
        .complete_host_interaction_notification(
            &foreign_notification_id,
            &admitted.notification_payload_digest,
            "foreign-notification-owner",
            0,
            "delivered",
            2,
            None,
        )
        .expect_err("notification completion must reject foreign storage key")
        .code()
        .contains("conflict"));
    assert!(store
        .reconcile_host_interaction_notification(
            &foreign_notification_id,
            &admitted.notification_payload_digest,
            "delivered",
            3,
            None,
        )
        .expect_err("notification reconciliation must reject foreign storage key")
        .code()
        .contains("conflict"));
    assert_eq!(
        connection
            .get::<_, String>(&foreign_notification_key)
            .expect("foreign notification remains unchanged"),
        original_notification
    );
    connection
        .del::<_, ()>(&foreign_notification_key)
        .expect("remove foreign notification key");
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
    assert_eq!(
        store
            .claim_controller_command_wake(
                &command.command_id,
                &command.command_digest,
                "redis-wake-owner-a",
                20_000,
                2,
            )
            .expect("same-owner wake replay")
            .expect("same-owner wake receipt"),
        claimed_wake
    );
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
