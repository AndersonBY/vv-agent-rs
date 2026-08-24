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
    store
        .create_checkpoint(source)
        .expect("create producer checkpoint");
    let claimed = store
        .claim_checkpoint(
            &handle.checkpoint_key,
            2,
            "claim-canonical",
            10_000,
            1,
            ClaimMode::Continue,
        )
        .expect("claim producer checkpoint")
        .expect("claimed producer checkpoint");
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
