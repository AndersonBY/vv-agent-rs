use super::*;

#[test]
fn context_defer_requires_checkpoint_and_preserves_opaque_identity() {
    let mut context = ToolContext::new(".");
    context.tool_call_id = "call_without_checkpoint".to_string();
    let outcome = context.defer();
    let ToolCallOutcome::Completed { result } = outcome else {
        panic!("non-durable context must not synthesize a handle");
    };
    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(
        result.error_code.as_deref(),
        Some("deferred_requires_checkpoint")
    );

    context.set_deferred_identity(
        "tenant-7/run-42",
        "op_tool_cycle_2_call_deferred",
        1,
        "ba0cefd88d9c971b57608e5d3defb147117eec2875872f9ff093aef016ced978",
    );
    let outcome = context.defer();
    let ToolCallOutcome::Deferred { handle } = outcome else {
        panic!("durable context should produce a deferred handle");
    };
    assert_eq!(handle.schema_version, "vv-agent.deferred-tool-handle.v2");
    assert_eq!(handle.checkpoint_key, "tenant-7/run-42");
    assert_eq!(handle.operation_id, "op_tool_cycle_2_call_deferred");
    handle.validate().expect("canonical handle");
}

#[test]
fn deferred_error_projects_the_canonical_operation_error() {
    let key = "memory-deferred-error";
    let digest = "e".repeat(64);
    let checkpoint =
        super::checkpoint_with_started_tools(key, &[("op_error", "call_error", &digest)]);
    let store = InMemoryCheckpointStore::new();
    let claimed = super::create_claimed_running_checkpoint(&store, checkpoint, "claim-error", 1);
    let handle = DeferredToolHandle::new(key, "op_error", 1, digest.clone()).expect("handle");
    store
        .admit_deferred_batch(
            key,
            claimed.revision,
            "claim-error",
            1,
            &[super::batch_entry(
                "op_error",
                "call_error",
                &digest,
                ToolCallOutcome::deferred(handle.clone()),
            )],
        )
        .expect("admission");

    let mut result = ToolExecutionResult::error("call_error", "");
    result.error_code = None;
    result.metadata.insert("retryable".to_string(), json!(true));
    assert!(matches!(
        store.resolve_deferred(handle, result).expect("resolution"),
        DeferredResolveDecision::AppliedReady { .. }
    ));
    let checkpoint = store
        .load_checkpoint(key)
        .expect("load")
        .expect("checkpoint");
    let error = checkpoint.tool_journal[0]
        .error
        .as_ref()
        .expect("operation error");
    assert_eq!(error.code, "tool_operation_failed");
    assert_eq!(error.message, "tool operation failed");
    assert!(error.retryable);
}

#[test]
fn deferred_admission_selects_the_complete_operation_identity() {
    let key = "memory-deferred-duplicate-operation";
    let first_digest = "a".repeat(64);
    let second_digest = "b".repeat(64);
    let mut first = started_tool("op_duplicate", "call_first", &first_digest);
    first.state = OperationState::Planned;
    let mut second = started_tool("op_duplicate", "call_second", &second_digest);
    second.attempt = 2;
    let mut checkpoint = minimal_checkpoint(key);
    checkpoint.tool_journal = vec![first, second];
    checkpoint
        .validate()
        .expect("duplicate operation identities are valid");

    let store = InMemoryCheckpointStore::new();
    let claimed =
        super::create_claimed_running_checkpoint(&store, checkpoint, "claim-duplicate", 1);
    let handle =
        DeferredToolHandle::new(key, "op_duplicate", 2, second_digest.clone()).expect("handle");
    let mut batch = batch_entry(
        "op_duplicate",
        "call_second",
        &second_digest,
        ToolCallOutcome::deferred(handle.clone()),
    );
    batch.attempt = 2;

    let admitted = store
        .admit_deferred_batch(key, claimed.revision, "claim-duplicate", 1, &[batch])
        .expect("admit complete identity");
    assert_eq!(
        admitted.checkpoint.status,
        vv_agent::CheckpointStatus::Deferred
    );
    assert_eq!(
        admitted.checkpoint.tool_journal[0].state,
        OperationState::Planned
    );
    assert_eq!(
        admitted.checkpoint.tool_journal[1].state,
        OperationState::Deferred
    );
    assert_eq!(admitted.handles, vec![handle]);
}

#[test]
fn deferred_wires_include_current_schema_and_reject_closed_shape_drift() {
    let handle =
        DeferredToolHandle::new("wire/checkpoint", "op_wire", 1, "a".repeat(64)).expect("handle");
    let outcome = ToolCallOutcome::deferred(handle.clone());
    let encoded = serde_json::to_value(&outcome).expect("outcome wire");
    assert_eq!(encoded["schema_version"], "vv-agent.tool-call-outcome.v2");
    assert_eq!(
        serde_json::from_value::<ToolCallOutcome>(encoded.clone()).expect("outcome round trip"),
        outcome
    );
    let mut unknown = encoded.clone();
    unknown["extra"] = json!(true);
    assert!(serde_json::from_value::<ToolCallOutcome>(unknown).is_err());
    let mut stale = encoded;
    stale["schema_version"] = json!("vv-agent.tool-call-outcome.v1");
    assert!(serde_json::from_value::<ToolCallOutcome>(stale).is_err());

    let decision = DeferredResolveDecision::not_admitted();
    let encoded = serde_json::to_value(&decision).expect("decision wire");
    assert_eq!(
        encoded["schema_version"],
        "vv-agent.deferred-resolve-decision.v1"
    );
    assert_eq!(
        serde_json::from_value::<DeferredResolveDecision>(encoded).expect("decision round trip"),
        decision
    );

    let mut malformed_handle = serde_json::to_value(&handle).expect("handle wire");
    malformed_handle["schema_version"] = json!("stale");
    assert!(serde_json::from_value::<DeferredToolHandle>(malformed_handle).is_err());
}

#[test]
fn canonical_receipt_and_event_jcs_vectors_are_produced_from_fixture_values() {
    let fixture: Value = serde_json::from_str(DEFERRED_FIXTURE).expect("deferred fixture");
    let canonical = &fixture["resolution"]["receipt_index"]["canonical_entry"];
    let handle: DeferredToolHandle =
        serde_json::from_value(canonical["handle"].clone()).expect("canonical handle");
    let result = ToolExecutionResult::from_dict(&canonical["result"]).expect("canonical result");
    assert_eq!(
        handle.handle_key().expect("handle key"),
        canonical["handle_key"]
    );
    assert_eq!(
        vv_agent::checkpoint::result_digest(&result).expect("result digest"),
        canonical["result_digest"]
    );

    let mut checkpoint = minimal_checkpoint(&handle.checkpoint_key);
    checkpoint.root_run_id = "run_deferred".to_string();
    checkpoint.trace_id = "trace_deferred".to_string();
    checkpoint.cycle_index = 1;
    let mut journal = started_tool(
        &handle.operation_id,
        &handle.operation_id["op_tool_cycle_2_".len()..],
        &handle.request_digest,
    );
    journal.cycle_index = 2;
    journal.state = OperationState::Deferred;
    journal.deferred_handle = Some(handle.clone());
    checkpoint.tool_journal = vec![journal.clone()];
    checkpoint
        .validate()
        .expect("canonical deferred checkpoint");
    let mut event = vv_agent::runtime::state::receipt_event(&checkpoint, &journal, &result)
        .expect("receipt event");
    event.event["created_at"] = fixture["resolution"]["receipt_index"]["golden_digest_vectors"][2]
        ["value"]["created_at"]
        .clone();
    assert_eq!(
        event.event["event_id"], canonical["event_id"],
        "stable completion event identity must be canonical"
    );
    assert_eq!(
        vv_agent::event_payload_digest(&event.event).expect("event digest"),
        canonical["event_payload_digest"]
    );

    // Exercise the real public producer path as well as the pure golden
    // vector helper above: create a claimed Started journal, admit the opaque
    // handle through the store CAS, then resolve it through the independent
    // receipt index.  The receipt must carry the exact canonical handle key
    // and result digest, while its event digests must match the durable event
    // actually written by resolution.
    let mut source = checkpoint.clone();
    source.status = vv_agent::CheckpointStatus::Running;
    source.tool_journal[0].state = OperationState::Started;
    source.tool_journal[0].deferred_handle = None;
    source.validate().expect("started producer checkpoint");
    let store = InMemoryCheckpointStore::new();
    let claimed = super::create_cycle_two_running_checkpoint(&store, source, "claim-canonical");
    let mut canonical_entry = batch_entry(
        &handle.operation_id,
        &result.tool_call_id,
        &handle.request_digest,
        ToolCallOutcome::deferred(handle.clone()),
    );
    canonical_entry.cycle_index = 2;
    let admission = store
        .admit_deferred_batch(
            &handle.checkpoint_key,
            claimed.revision,
            "claim-canonical",
            2,
            &[canonical_entry],
        )
        .expect("admit canonical producer handle");
    let DeferredResolveDecision::AppliedReady { receipt } = store
        .resolve_deferred(handle.clone(), result.clone())
        .expect("resolve canonical producer handle")
    else {
        panic!("canonical resolution must release the last barrier");
    };
    assert_eq!(receipt.handle_key, canonical["handle_key"]);
    assert_eq!(receipt.result_digest, canonical["result_digest"]);
    assert_eq!(receipt.event_id, canonical["event_id"]);
    assert_eq!(receipt.handle, handle);
    assert_eq!(receipt.result, result);
    let resolved_checkpoint = store
        .load_checkpoint(&admission.checkpoint.checkpoint_key)
        .expect("load resolved producer checkpoint")
        .expect("resolved producer checkpoint");
    let completed_event = resolved_checkpoint
        .event_outbox
        .iter()
        .find(|entry| entry.event_id == receipt.event_id)
        .expect("resolved receipt event");
    assert_eq!(receipt.event_payload_digest, completed_event.payload_digest);
    assert_eq!(
        vv_agent::event_payload_digest(&completed_event.event).expect("resolved event digest"),
        receipt.event_payload_digest
    );
}

#[test]
fn receipt_and_checkpoint_readers_recompute_closed_identities() {
    let fixture: Value = serde_json::from_str(DEFERRED_FIXTURE).expect("deferred fixture");
    let canonical = &fixture["resolution"]["receipt_index"]["canonical_entry"];
    let mut malformed_receipt = canonical.clone();
    malformed_receipt["event_id"] = json!(format!("evt_receipt_{}", "0".repeat(64)));
    let receipt_error = serde_json::from_value::<vv_agent::DeferredReceipt>(malformed_receipt)
        .expect_err("receipt reader must reject a mismatched canonical event identity");
    assert!(receipt_error
        .to_string()
        .contains("deferred_receipt_identity_invalid"));

    let codec: Value = serde_json::from_str(CHECKPOINT_FIXTURE).expect("checkpoint fixture");
    let mut terminal = codec["valid_cases"]
        .as_array()
        .expect("valid cases")
        .iter()
        .find(|case| case["name"] == "operator_abort_terminal_retains_closed_tool_entries")
        .expect("closed tool terminal case")["payload"]
        .clone();
    terminal["tool_journal"][0]["identity_key"] = json!("0".repeat(64));
    let terminal_error = checkpoint_from_json(
        &serde_json::to_string(&terminal).expect("terminal JSON"),
        262_144,
    )
    .expect_err("checkpoint reader must recompute terminal tool identity");
    assert_eq!(terminal_error.code(), "operation_receipt_identity_invalid");

    let key = "deferred-reader-checkpoint";
    let digest = "a".repeat(64);
    let mut deferred = minimal_checkpoint(key);
    let mut journal = started_tool("op_deferred", "call_deferred", &digest);
    journal.state = OperationState::Deferred;
    journal.deferred_handle = Some(
        DeferredToolHandle::new("different-checkpoint", "op_deferred", 1, digest)
            .expect("deferred handle"),
    );
    deferred.status = vv_agent::CheckpointStatus::Deferred;
    deferred.tool_journal = vec![journal];
    let deferred_error = deferred
        .validate()
        .expect_err("checkpoint reader must bind all deferred handle identity fields");
    assert_eq!(deferred_error.code(), "checkpoint_status_invalid");
}

#[test]
fn sqlite_receipt_row_checkpoint_key_must_match_embedded_handle() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("receipt-identity.sqlite");
    let store = SqliteCheckpointStore::new(&path).expect("sqlite");
    let key = "sqlite-receipt-identity";
    let other_key = "sqlite-receipt-other";
    let digest = "b".repeat(64);
    let mut checkpoint = minimal_checkpoint(key);
    checkpoint.tool_journal = vec![started_tool("op_sqlite", "call_sqlite", &digest)];
    checkpoint.validate().expect("started checkpoint");
    let claimed = create_claimed_running_checkpoint(&store, checkpoint, "claim-sqlite", 1);
    let other = initial_checkpoint(minimal_checkpoint(other_key));
    assert!(store.create_checkpoint(other).expect("other checkpoint"));
    let handle = DeferredToolHandle::new(key, "op_sqlite", 1, digest.clone()).expect("handle");
    store
        .admit_deferred_batch(
            key,
            claimed.revision,
            "claim-sqlite",
            1,
            &[batch_entry(
                "op_sqlite",
                "call_sqlite",
                &digest,
                ToolCallOutcome::deferred(handle.clone()),
            )],
        )
        .expect("admission");
    let result = ToolExecutionResult::success("call_sqlite", "accepted");
    let receipt = match store
        .resolve_deferred(handle.clone(), result.clone())
        .expect("resolution")
    {
        DeferredResolveDecision::AppliedReady { receipt } => receipt,
        other => panic!("unexpected resolution: {other:?}"),
    };
    let connection = rusqlite::Connection::open(&path).expect("open sqlite row");
    connection
        .execute(
            "UPDATE deferred_resolution_receipts SET checkpoint_key = ?1 WHERE handle_key = ?2",
            rusqlite::params![other_key, receipt.handle_key],
        )
        .expect("tamper checkpoint index column");
    let error = store
        .resolve_deferred(handle, result)
        .expect_err("row checkpoint key mismatch must fail closed");
    assert_eq!(error.code(), "deferred_receipt_identity_invalid");
    store
        .delete_checkpoint(key)
        .expect("delete source checkpoint");
    store
        .delete_checkpoint(other_key)
        .expect("delete other checkpoint");
}

#[test]
#[ignore = "requires a local Redis instance"]
fn redis_receipt_index_must_match_embedded_handle() {
    let url = std::env::var("VV_AGENT_TEST_REDIS_URL")
        .unwrap_or_else(|_| "redis://127.0.0.1/".to_string());
    let store = RedisCheckpointStore::new(&url).expect("redis");
    let key = format!("redis-receipt-identity-{}", uuid::Uuid::new_v4().simple());
    let other_key = format!("redis-receipt-other-{}", uuid::Uuid::new_v4().simple());
    let digest = tool_request_digest("call_redis_identity", "remote_write", &json!({}), None)
        .expect("request digest");
    let checkpoint = checkpoint_with_started_tools(
        &key,
        &[("op_redis_identity", "call_redis_identity", &digest)],
    );
    let claimed = create_claimed_running_checkpoint(&store, checkpoint, "claim-redis", 1);
    assert!(store
        .create_checkpoint(initial_checkpoint(minimal_checkpoint(&other_key)))
        .expect("other checkpoint"));
    let handle =
        DeferredToolHandle::new(&key, "op_redis_identity", 1, digest.clone()).expect("handle");
    store
        .admit_deferred_batch(
            &key,
            claimed.revision,
            "claim-redis",
            1,
            &[batch_entry(
                "op_redis_identity",
                "call_redis_identity",
                &digest,
                ToolCallOutcome::deferred(handle.clone()),
            )],
        )
        .expect("admission");
    let result = ToolExecutionResult::success("call_redis_identity", "accepted");
    let receipt = match store
        .resolve_deferred(handle.clone(), result.clone())
        .expect("resolution")
    {
        DeferredResolveDecision::AppliedReady { receipt } => receipt,
        other => panic!("unexpected resolution: {other:?}"),
    };
    let client = redis::Client::open(url.as_str()).expect("redis client");
    let mut connection = client.get_connection().expect("redis connection");
    let receipt_key = RedisCheckpointStore::deferred_receipt_key(&receipt.handle_key);
    let other_set = RedisCheckpointStore::deferred_receipts_checkpoint_set_key(&other_key);
    redis::Commands::sadd::<_, _, ()>(&mut connection, &other_set, &receipt_key)
        .expect("cross-index receipt");
    let error = store
        .delete_checkpoint(&other_key)
        .expect_err("cross-checkpoint Redis index must fail closed");
    assert_eq!(error.code(), "deferred_receipt_identity_invalid");
    assert!(matches!(
        store
            .resolve_deferred(handle, result)
            .expect("receipt replay after rejected cleanup"),
        DeferredResolveDecision::Replayed { .. }
    ));
    redis::Commands::srem::<_, _, ()>(&mut connection, &other_set, &receipt_key)
        .expect("remove cross-index receipt");
    store
        .delete_checkpoint(&key)
        .expect("delete source checkpoint");
    store
        .delete_checkpoint(&other_key)
        .expect("delete other checkpoint");
}
