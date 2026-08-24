use std::collections::BTreeMap;
use std::sync::{Arc, Barrier};
use std::thread;

use serde_json::{json, Value};
use vv_agent::{
    checkpoint_from_json, tool_request_digest, AcceptDeferredDecision, Checkpoint, CheckpointStore,
    ClaimMode, DeferredBatchEntry, DeferredResolveDecision, DeferredToolHandle, EventOutboxEntry,
    InMemoryCheckpointStore, ModelCallOperation, ModelCallRecord, ModelCallStatus,
    OperationJournalEntry, OperationState, RedisCheckpointStore, RunEvent, SqliteCheckpointStore,
    TokenUsage, ToolCall, ToolCallOutcome, ToolContext, ToolExecutionResult, ToolIdempotency,
    ToolOrchestrator, ToolResultStatus, ToolRunOptions, ToolSpec, ToolSpecExecutor,
};

const CHECKPOINT_FIXTURE: &str = include_str!("fixtures/parity/checkpoint_codec.json");
const DEFERRED_FIXTURE: &str = include_str!("fixtures/parity/deferred_tool.json");

fn minimal_checkpoint(key: &str) -> Checkpoint {
    let fixture: Value = serde_json::from_str(CHECKPOINT_FIXTURE).expect("checkpoint fixture");
    let mut payload = fixture["valid_cases"]
        .as_array()
        .expect("valid cases")
        .iter()
        .find(|case| case["name"] == "minimal_running")
        .expect("minimal checkpoint case")["payload"]
        .clone();
    payload["checkpoint_key"] = json!(key);
    checkpoint_from_json(
        &serde_json::to_string(&payload).expect("checkpoint JSON"),
        262_144,
    )
    .expect("valid checkpoint")
}

fn started_tool(
    operation_id: &str,
    tool_call_id: &str,
    request_digest: &str,
) -> OperationJournalEntry {
    let mut entry = OperationJournalEntry::tool(
        operation_id,
        1,
        1,
        request_digest,
        tool_call_id,
        "remote_write",
        BTreeMap::new().into_iter().collect(),
        None,
        ToolIdempotency::Unsupported,
    );
    entry.state = OperationState::Started;
    entry
}

fn checkpoint_with_started_tools(key: &str, operations: &[(&str, &str, &str)]) -> Checkpoint {
    let mut checkpoint = minimal_checkpoint(key);
    checkpoint.tool_journal = operations
        .iter()
        .map(|(operation_id, tool_call_id, digest)| {
            started_tool(operation_id, tool_call_id, digest)
        })
        .collect();
    checkpoint.validate().expect("started checkpoint");
    checkpoint
}

fn batch_entry(
    operation_id: &str,
    tool_call_id: &str,
    digest: &str,
    outcome: ToolCallOutcome,
) -> DeferredBatchEntry {
    DeferredBatchEntry {
        operation_id: operation_id.to_string(),
        cycle_index: 1,
        attempt: 1,
        request_digest: digest.to_string(),
        tool_call_id: tool_call_id.to_string(),
        tool_name: "remote_write".to_string(),
        idempotency_key: None,
        idempotency_support: ToolIdempotency::Unsupported,
        outcome: match outcome {
            ToolCallOutcome::Deferred { handle } => ToolCallOutcome::deferred(handle),
            ToolCallOutcome::Completed { result } => ToolCallOutcome::completed(result),
        },
    }
}

fn admitted_memory_checkpoint(
    key: &str,
) -> (
    InMemoryCheckpointStore,
    Checkpoint,
    DeferredToolHandle,
    ToolExecutionResult,
) {
    let digest_a = "a".repeat(64);
    let digest_b = "b".repeat(64);
    let checkpoint = checkpoint_with_started_tools(
        key,
        &[
            ("op_deferred", "call_deferred", &digest_a),
            ("op_completed", "call_completed", &digest_b),
        ],
    );
    let store = InMemoryCheckpointStore::new();
    assert!(store.create_checkpoint(checkpoint.clone()).expect("create"));
    let claimed = store
        .claim_checkpoint(key, 1, "claim-1", 10_000, 1, ClaimMode::Continue)
        .expect("claim")
        .expect("claimed checkpoint");
    let handle = DeferredToolHandle::new(key, "op_deferred", 1, digest_a).expect("deferred handle");
    let completed = ToolExecutionResult::success("call_completed", "ordinary success");
    let admission = store
        .admit_deferred_batch(
            key,
            claimed.revision,
            "claim-1",
            1,
            &[
                batch_entry(
                    "op_deferred",
                    "call_deferred",
                    &"a".repeat(64),
                    ToolCallOutcome::deferred(handle.clone()),
                ),
                batch_entry(
                    "op_completed",
                    "call_completed",
                    &"b".repeat(64),
                    ToolCallOutcome::completed(completed.clone()),
                ),
            ],
        )
        .expect("atomic admission");
    assert_eq!(admission.handles, vec![handle.clone()]);
    assert_eq!(
        admission.checkpoint.status,
        vv_agent::CheckpointStatus::Deferred
    );
    assert_eq!(admission.checkpoint.claim_token, None);
    assert_eq!(admission.checkpoint.revision, claimed.revision + 1);
    assert_eq!(
        admission
            .checkpoint
            .tool_journal
            .iter()
            .filter(|entry| entry.state == OperationState::Deferred)
            .count(),
        1
    );
    assert_eq!(admission.checkpoint.event_outbox.len(), 2);
    let deferred_event = admission
        .checkpoint
        .event_outbox
        .iter()
        .find(|entry| entry.event["type"] == "tool_call_deferred")
        .expect("deferred admission event");
    assert_eq!(deferred_event.event["checkpoint_key"], key);
    assert_eq!(deferred_event.event["operation_kind"], "tool");
    (store, admission.checkpoint, handle, completed)
}

#[tokio::test]
async fn tool_spec_handler_produces_deferred_outcome_without_model_result() {
    let spec = ToolSpec::new(
        "remote_write",
        "Write to an external service.",
        Arc::new(|context, _arguments| {
            let _ = context.defer();
            ToolExecutionResult::success("call_deferred", "must not be model-visible")
        }),
    );
    let orchestrator = ToolOrchestrator::from_tools(vec![ToolSpecExecutor::new(spec).into_arc()]);
    let mut context = ToolContext::new(".");
    context.set_deferred_identity("tenant-7/run-42", "op_deferred", 1, "a".repeat(64));
    let outcome = orchestrator
        .run_one_outcome(
            ToolCall::new(
                "call_deferred",
                "remote_write",
                BTreeMap::new().into_iter().collect(),
            ),
            &mut context,
            ToolRunOptions::default(),
        )
        .await
        .expect("tool outcome");
    let ToolCallOutcome::Deferred { handle } = outcome else {
        panic!("tool context marker must produce Deferred");
    };
    assert_eq!(handle.operation_id, "op_deferred");
}

#[path = "deferred_tools/canonical.rs"]
mod canonical;

#[test]
fn memory_store_admission_resolution_replay_conflict_and_barrier() {
    let (store, admitted, handle, completed) = admitted_memory_checkpoint("memory-deferred");
    let before_mismatch = store
        .load_checkpoint("memory-deferred")
        .expect("load before mismatch")
        .expect("checkpoint before mismatch");
    let mismatch = store
        .resolve_deferred(
            handle.clone(),
            ToolExecutionResult::success("wrong-tool-call", "must not write"),
        )
        .expect_err("tool_call_id mismatch must be rejected");
    assert_eq!(mismatch.code(), "deferred_resolution_stale");
    let after_mismatch = store
        .load_checkpoint("memory-deferred")
        .expect("load after mismatch")
        .expect("checkpoint after mismatch");
    assert_eq!(after_mismatch.revision, before_mismatch.revision);
    assert_eq!(after_mismatch.event_outbox, before_mismatch.event_outbox);
    for status in [
        ToolResultStatus::WaitResponse,
        ToolResultStatus::Running,
        ToolResultStatus::PendingCompress,
    ] {
        let mut non_definitive = ToolExecutionResult::success("call_deferred", "not definitive");
        non_definitive.status = status;
        let error = store
            .resolve_deferred(handle.clone(), non_definitive)
            .expect_err("non-definitive resolution must be rejected");
        assert_eq!(error.code(), "deferred_resolution_result_invalid");
        let retained = store
            .load_checkpoint("memory-deferred")
            .expect("load after non-definitive rejection")
            .expect("checkpoint after non-definitive rejection");
        assert_eq!(retained.revision, before_mismatch.revision);
        assert_eq!(retained.event_outbox, before_mismatch.event_outbox);
    }
    let resolved = store
        .resolve_deferred(
            handle.clone(),
            ToolExecutionResult::success("call_deferred", "accepted"),
        )
        .expect("resolution");
    let DeferredResolveDecision::AppliedReady { receipt } = resolved else {
        panic!("last barrier resolution must be ready");
    };
    assert_eq!(
        receipt.result,
        ToolExecutionResult::success("call_deferred", "accepted")
    );
    let after = store
        .load_checkpoint("memory-deferred")
        .expect("load")
        .expect("checkpoint");
    assert_eq!(after.status, vv_agent::CheckpointStatus::Running);
    assert_eq!(after.revision, admitted.revision + 1);
    assert_eq!(
        after
            .tool_journal
            .iter()
            .find(|entry| entry.operation_id == "op_completed")
            .expect("ordinary entry")
            .state,
        OperationState::Succeeded
    );

    let replay = store
        .resolve_deferred(
            handle.clone(),
            ToolExecutionResult::success("call_deferred", "accepted"),
        )
        .expect("replay");
    assert!(matches!(replay, DeferredResolveDecision::Replayed { .. }));
    assert_eq!(
        store
            .load_checkpoint("memory-deferred")
            .expect("load")
            .expect("checkpoint")
            .revision,
        after.revision
    );
    let conflict = store
        .resolve_deferred(
            handle,
            ToolExecutionResult::error("call_deferred", "different"),
        )
        .expect_err("different result must conflict");
    assert_eq!(conflict.code(), "deferred_resolution_conflict");
    assert_eq!(completed.status, ToolResultStatus::Success);
}

#[test]
fn memory_store_returns_not_admitted_reconciliation_and_stale_decisions() {
    let key = "memory-early";
    let digest = "c".repeat(64);
    let handle = DeferredToolHandle::new(key, "op_started", 1, digest.clone()).expect("handle");
    let store = InMemoryCheckpointStore::new();
    let checkpoint = checkpoint_with_started_tools(key, &[("op_started", "call_started", &digest)]);
    store.create_checkpoint(checkpoint).expect("create");
    let result = ToolExecutionResult::success("call_started", "early");
    let decision = store
        .resolve_deferred(handle.clone(), result.clone())
        .expect("early callback");
    assert!(matches!(
        decision,
        DeferredResolveDecision::NotAdmitted { .. }
    ));
    assert_eq!(
        store
            .load_checkpoint(key)
            .expect("load")
            .expect("checkpoint")
            .revision,
        0
    );

    let mut ambiguous_checkpoint = checkpoint_with_started_tools(
        "memory-ambiguous",
        &[("op_ambiguous", "call_ambiguous", &"d".repeat(64))],
    );
    ambiguous_checkpoint.tool_journal[0].state = OperationState::Ambiguous;
    ambiguous_checkpoint.status = vv_agent::CheckpointStatus::ReconciliationRequired;
    ambiguous_checkpoint
        .validate()
        .expect("ambiguous checkpoint");
    let ambiguous_store = InMemoryCheckpointStore::new();
    ambiguous_store
        .create_checkpoint(ambiguous_checkpoint)
        .expect("create ambiguous");
    let ambiguous_handle =
        DeferredToolHandle::new("memory-ambiguous", "op_ambiguous", 1, "d".repeat(64))
            .expect("ambiguous handle");
    let decision = ambiguous_store
        .resolve_deferred(ambiguous_handle, result)
        .expect("ambiguous callback");
    assert!(matches!(
        decision,
        DeferredResolveDecision::ReconciliationRequired
    ));

    let stale = store
        .resolve_deferred(
            DeferredToolHandle::new(key, "missing", 1, "e".repeat(64)).expect("stale handle"),
            ToolExecutionResult::success("missing", "stale"),
        )
        .expect_err("unknown handle must be stale");
    assert_eq!(stale.code(), "deferred_resolution_stale");
}

#[test]
fn real_producer_claimed_checkpoint_resolution_is_typed_error_without_writes() {
    let fixture: Value = serde_json::from_str(DEFERRED_FIXTURE).expect("deferred fixture");
    let canonical = &fixture["resolution"]["receipt_index"]["canonical_entry"];
    let handle: DeferredToolHandle =
        serde_json::from_value(canonical["handle"].clone()).expect("canonical handle");
    let result = ToolExecutionResult::from_dict(&canonical["result"]).expect("canonical result");

    // Build the deferred barrier through the public create, claim, and
    // admission producers.  The claimed snapshot below models the live
    // worker race after admission: validation permits the running barrier,
    // while no claim path can acquire a deferred checkpoint itself.
    let mut checkpoint = minimal_checkpoint(&handle.checkpoint_key);
    checkpoint.cycle_index = 1;
    let mut journal = started_tool(
        &handle.operation_id,
        &result.tool_call_id,
        &handle.request_digest,
    );
    journal.cycle_index = 2;
    checkpoint.tool_journal = vec![journal];
    checkpoint.validate().expect("started checkpoint");

    let store = InMemoryCheckpointStore::new();
    store
        .create_checkpoint(checkpoint)
        .expect("create checkpoint");
    let claimed = store
        .claim_checkpoint(
            &handle.checkpoint_key,
            2,
            "claim-admission",
            10_000,
            1,
            ClaimMode::Continue,
        )
        .expect("claim checkpoint")
        .expect("claimed checkpoint");
    let admission = store
        .admit_deferred_batch(
            &handle.checkpoint_key,
            claimed.revision,
            "claim-admission",
            2,
            &[DeferredBatchEntry {
                operation_id: handle.operation_id.clone(),
                cycle_index: 2,
                attempt: handle.attempt,
                request_digest: handle.request_digest.clone(),
                tool_call_id: result.tool_call_id.clone(),
                tool_name: "remote_write".to_string(),
                idempotency_key: None,
                idempotency_support: ToolIdempotency::Unsupported,
                outcome: ToolCallOutcome::deferred(handle.clone()),
            }],
        )
        .expect("deferred admission");
    assert_eq!(
        admission.checkpoint.status,
        vv_agent::CheckpointStatus::Deferred
    );
    assert!(admission.checkpoint.claim_token.is_none());

    // Simulate the authoritative record observed while another worker owns
    // the claim.  This is deliberately a public-store save of the validated
    // running-with-deferred-barrier shape, not a private index mutation.
    let mut owned = admission.checkpoint.clone();
    owned.status = vv_agent::CheckpointStatus::Running;
    owned.claim_token = Some("owner-b".to_string());
    owned.claimed_cycle = Some(owned.cycle_index + 1);
    owned.lease_expires_at_ms = Some(10_000);
    owned.revision += 1;
    owned.validate().expect("claimed running barrier");
    store.save_checkpoint(owned).expect("save claimed snapshot");

    let before = store
        .load_checkpoint(&handle.checkpoint_key)
        .expect("load before")
        .expect("checkpoint before");
    let error = store
        .resolve_deferred(handle.clone(), result.clone())
        .expect_err("claimed checkpoint must reject resolution");
    assert_eq!(error.code(), "deferred_checkpoint_claimed");
    let after = store
        .load_checkpoint(&handle.checkpoint_key)
        .expect("load after")
        .expect("checkpoint after");
    assert_eq!(after, before, "claimed rejection must perform zero writes");

    // A subsequent unclaimed resolution must apply rather than replay, which
    // is producer evidence that the rejected call did not create a receipt.
    let mut unclaimed = before;
    unclaimed.status = vv_agent::CheckpointStatus::Deferred;
    unclaimed.claim_token = None;
    unclaimed.claimed_cycle = None;
    unclaimed.lease_expires_at_ms = None;
    unclaimed.validate().expect("unclaimed deferred barrier");
    store
        .save_checkpoint(unclaimed)
        .expect("restore unclaimed barrier");
    assert!(matches!(
        store
            .resolve_deferred(handle, result)
            .expect("resolution after claim release"),
        DeferredResolveDecision::AppliedReady { .. }
    ));
}

#[test]
fn memory_store_linearizes_concurrent_resolution_and_replays_losers() {
    let (store, _admitted, handle, _completed) = admitted_memory_checkpoint("memory-concurrent");
    let store = Arc::new(store);
    let mut workers = Vec::new();
    for _ in 0..8 {
        let store = store.clone();
        let handle = handle.clone();
        workers.push(thread::spawn(move || {
            store.resolve_deferred(
                handle,
                ToolExecutionResult::success("call_deferred", "accepted"),
            )
        }));
    }
    let mut applied = 0;
    let mut replayed = 0;
    for worker in workers {
        match worker.join().expect("resolution worker") {
            Ok(DeferredResolveDecision::AppliedReady { .. }) => applied += 1,
            Ok(DeferredResolveDecision::Replayed { .. }) => replayed += 1,
            other => panic!("unexpected concurrent resolution result: {other:?}"),
        }
    }
    assert_eq!(applied, 1);
    assert_eq!(replayed, 7);
}

#[test]
fn memory_cleanup_and_resolution_race_leaves_no_receipt_or_checkpoint_orphan() {
    let (store, _admitted, handle, _completed) = admitted_memory_checkpoint("memory-cleanup-race");
    let store = Arc::new(store);
    let barrier = Arc::new(Barrier::new(2));
    let resolver_store = store.clone();
    let resolver_handle = handle.clone();
    let resolver_barrier = barrier.clone();
    let resolver = thread::spawn(move || {
        resolver_barrier.wait();
        resolver_store.resolve_deferred(
            resolver_handle,
            ToolExecutionResult::success("call_deferred", "accepted"),
        )
    });
    let cleanup_store = store.clone();
    let cleanup_barrier = barrier;
    let cleanup = thread::spawn(move || {
        cleanup_barrier.wait();
        cleanup_store.delete_checkpoint("memory-cleanup-race")
    });
    let resolution = resolver.join().expect("resolution worker");
    cleanup.join().expect("cleanup worker").expect("cleanup");
    assert!(matches!(
        resolution,
        Ok(DeferredResolveDecision::AppliedReady { .. }) | Err(_)
    ));
    assert!(store
        .load_checkpoint("memory-cleanup-race")
        .expect("load after cleanup")
        .is_none());
    let retry = store
        .resolve_deferred(
            handle,
            ToolExecutionResult::success("call_deferred", "accepted"),
        )
        .expect_err("cleanup must remove any receipt created by the racing resolver");
    assert_eq!(retry.code(), "deferred_resolution_stale");
}

#[test]
fn sqlite_store_keeps_receipts_independent_and_cleans_them_with_checkpoint() {
    let directory = tempfile::tempdir().expect("tempdir");
    let store =
        SqliteCheckpointStore::new(directory.path().join("deferred.sqlite")).expect("sqlite");
    let mut checkpoint = minimal_checkpoint("sqlite-deferred");
    let digest = "f".repeat(64);
    checkpoint.tool_journal = vec![started_tool("op_sqlite", "call_sqlite", &digest)];
    checkpoint.validate().expect("sqlite checkpoint");
    store.create_checkpoint(checkpoint).expect("create");
    let claimed = store
        .claim_checkpoint(
            "sqlite-deferred",
            1,
            "claim-sqlite",
            10_000,
            1,
            ClaimMode::Continue,
        )
        .expect("claim")
        .expect("claimed");
    let handle = DeferredToolHandle::new("sqlite-deferred", "op_sqlite", 1, digest.clone())
        .expect("sqlite handle");
    store
        .admit_deferred_batch(
            "sqlite-deferred",
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
        .expect("sqlite admission");
    let result = ToolExecutionResult::success("call_sqlite", "accepted");
    let mismatch = store
        .resolve_deferred(
            handle.clone(),
            ToolExecutionResult::success("wrong-tool-call", "must not write"),
        )
        .expect_err("sqlite tool_call_id mismatch must be rejected");
    assert_eq!(mismatch.code(), "deferred_resolution_stale");
    let before_resolution = store
        .load_checkpoint("sqlite-deferred")
        .expect("load before sqlite mismatch")
        .expect("sqlite checkpoint before mismatch");
    assert!(matches!(
        store
            .resolve_deferred(handle.clone(), result.clone())
            .expect("sqlite resolution"),
        DeferredResolveDecision::AppliedReady { .. }
    ));
    assert_eq!(
        before_resolution.revision + 1,
        store
            .load_checkpoint("sqlite-deferred")
            .expect("load after sqlite resolution")
            .expect("sqlite checkpoint after resolution")
            .revision
    );
    assert!(matches!(
        store
            .resolve_deferred(handle, result)
            .expect("sqlite replay"),
        DeferredResolveDecision::Replayed { .. }
    ));
    store
        .delete_checkpoint("sqlite-deferred")
        .expect("delete checkpoint");
    assert!(store
        .load_checkpoint("sqlite-deferred")
        .expect("load deleted")
        .is_none());
}

#[test]
#[ignore = "requires a local Redis instance"]
fn redis_store_rejects_result_tool_call_id_mismatch_without_a_write() {
    let url =
        std::env::var("VV_AGENT_REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1/".to_string());
    let store = RedisCheckpointStore::new(url).expect("redis");
    let key = format!("deferred-tools-redis-{}", uuid::Uuid::new_v4().simple());
    let digest = tool_request_digest("call_redis", "remote_write", &json!({}), None)
        .expect("canonical redis tool request digest");
    let checkpoint = checkpoint_with_started_tools(&key, &[("op_redis", "call_redis", &digest)]);
    checkpoint.validate().expect("redis checkpoint");
    store.create_checkpoint(checkpoint).expect("create");
    let claimed = store
        .claim_checkpoint(&key, 1, "claim-redis", 10_000, 1, ClaimMode::Continue)
        .expect("claim")
        .expect("claimed");
    let handle = DeferredToolHandle::new(&key, "op_redis", 1, digest.clone()).expect("handle");
    store
        .admit_deferred_batch(
            &key,
            claimed.revision,
            "claim-redis",
            1,
            &[batch_entry(
                "op_redis",
                "call_redis",
                &digest,
                ToolCallOutcome::deferred(handle.clone()),
            )],
        )
        .expect("admission");
    let before = store
        .load_checkpoint(&key)
        .expect("load before mismatch")
        .expect("checkpoint before mismatch");
    let error = store
        .resolve_deferred(
            handle,
            ToolExecutionResult::success("wrong-tool-call", "must not write"),
        )
        .expect_err("tool_call_id mismatch");
    assert_eq!(error.code(), "deferred_resolution_stale");
    let after = store
        .load_checkpoint(&key)
        .expect("load after mismatch")
        .expect("checkpoint after mismatch");
    assert_eq!(after.revision, before.revision);
    store.delete_checkpoint(&key).expect("cleanup");
}

#[test]
fn accept_deferred_adopts_ambiguous_entry_once_and_releases_recovery_claim() {
    let key = "memory-accept-deferred";
    let digest = "9".repeat(64);
    let mut checkpoint =
        checkpoint_with_started_tools(key, &[("op_accept", "call_accept", &digest)]);
    checkpoint.tool_journal[0].state = OperationState::Ambiguous;
    checkpoint.status = vv_agent::CheckpointStatus::ReconciliationRequired;
    checkpoint.validate().expect("reconciliation checkpoint");
    let store = InMemoryCheckpointStore::new();
    store.create_checkpoint(checkpoint).expect("create");
    let claimed = store
        .claim_checkpoint(key, 1, "claim-recovery", 10_000, 1, ClaimMode::Recovery)
        .expect("recovery claim")
        .expect("claimed");
    let handle = DeferredToolHandle::new(key, "op_accept", 1, digest).expect("accept handle");
    let admission = store
        .accept_deferred_batch(
            key,
            claimed.revision,
            "claim-recovery",
            1,
            &[AcceptDeferredDecision::new(handle.clone())],
        )
        .expect("accept deferred");
    assert_eq!(
        admission.checkpoint.status,
        vv_agent::CheckpointStatus::Deferred
    );
    assert!(admission.checkpoint.claim_token.is_none());
    assert_eq!(admission.checkpoint.event_outbox.len(), 2);
    let replay = store
        .accept_deferred_batch(
            key,
            admission.checkpoint.revision,
            "not-needed",
            1,
            &[AcceptDeferredDecision::new(handle)],
        )
        .expect("idempotent accept replay");
    assert_eq!(replay.checkpoint.revision, admission.checkpoint.revision);
}

#[test]
fn recovery_rejects_partial_acceptance_when_model_and_multiple_tools_are_ambiguous() {
    let key = "memory-recovery-model-and-tools";
    let digest_a = "a".repeat(64);
    let digest_b = "b".repeat(64);
    let mut checkpoint = checkpoint_with_started_tools(
        key,
        &[
            ("op_recovery_a", "call_recovery_a", &digest_a),
            ("op_recovery_b", "call_recovery_b", &digest_b),
        ],
    );
    for entry in &mut checkpoint.tool_journal {
        entry.state = OperationState::Ambiguous;
    }
    let operation_id = "op_model_recovery";
    let call_id = "op_model_recovery:attempt:1";
    let request_digest = "c".repeat(64);
    let mut model_entry = OperationJournalEntry::model(
        operation_id,
        1,
        1,
        request_digest,
        ModelCallOperation::AgentCycle,
        "test",
        "test-model",
        call_id,
    );
    model_entry.state = OperationState::Ambiguous;
    let usage = TokenUsage::default();
    checkpoint.model_call_journal = vec![model_entry];
    checkpoint.model_calls = vec![ModelCallRecord {
        call_id: call_id.to_string(),
        operation_id: operation_id.to_string(),
        attempt: 1,
        operation: ModelCallOperation::AgentCycle,
        cycle_index: 1,
        backend: "test".to_string(),
        model: "test-model".to_string(),
        status: ModelCallStatus::Ambiguous,
        usage: usage.clone(),
        error_code: Some("model_outcome_ambiguous".to_string()),
    }];
    let started = RunEvent::model_call_started(
        &checkpoint.root_run_id,
        &checkpoint.trace_id,
        &checkpoint.task_id,
        1,
        call_id,
        operation_id,
        1,
        ModelCallOperation::AgentCycle,
        "test",
        "test-model",
    );
    let failed = RunEvent::model_call_failed(
        &checkpoint.root_run_id,
        &checkpoint.trace_id,
        &checkpoint.task_id,
        1,
        call_id,
        operation_id,
        1,
        ModelCallOperation::AgentCycle,
        "test",
        "test-model",
        serde_json::from_value(json!("ambiguous")).expect("failure outcome"),
        usage,
        "model_outcome_ambiguous",
    );
    for event in [started, failed] {
        checkpoint.event_outbox.push(
            EventOutboxEntry::pending(
                event.event_id().as_str().to_string(),
                serde_json::to_value(event).expect("model event wire"),
            )
            .expect("model event outbox"),
        );
    }
    checkpoint.status = vv_agent::CheckpointStatus::ReconciliationRequired;
    checkpoint
        .validate()
        .expect("model and tool ambiguity checkpoint");

    let store = InMemoryCheckpointStore::new();
    store
        .create_checkpoint(checkpoint)
        .expect("create ambiguity checkpoint");
    let claimed = store
        .claim_checkpoint(
            key,
            1,
            "claim-model-and-tools",
            10_000,
            1,
            ClaimMode::Recovery,
        )
        .expect("recovery claim")
        .expect("claimed ambiguity checkpoint");
    let decisions = vec![
        AcceptDeferredDecision::new(
            DeferredToolHandle::new(key, "op_recovery_a", 1, digest_a).expect("handle a"),
        ),
        AcceptDeferredDecision::new(
            DeferredToolHandle::new(key, "op_recovery_b", 1, digest_b).expect("handle b"),
        ),
    ];
    let before = store
        .load_checkpoint(key)
        .expect("load before partial acceptance")
        .expect("checkpoint before partial acceptance");
    let error = store
        .accept_deferred_batch(
            key,
            claimed.revision,
            "claim-model-and-tools",
            1,
            &decisions,
        )
        .expect_err("model ambiguity must prevent partial tool acceptance");
    assert_eq!(error.code(), "reconciliation_required");
    let after = store
        .load_checkpoint(key)
        .expect("load after partial rejection")
        .expect("checkpoint after partial rejection");
    assert_eq!(after, before, "rejection must not partially adopt tools");
}

#[test]
fn accept_deferred_multi_handle_replay_is_exact_and_rejects_subset_duplicate_or_wrong() {
    let key = "memory-accept-deferred-multi";
    let digest_a = "a".repeat(64);
    let digest_b = "b".repeat(64);
    let mut checkpoint = checkpoint_with_started_tools(
        key,
        &[
            ("op_accept_a", "call_accept_a", &digest_a),
            ("op_accept_b", "call_accept_b", &digest_b),
        ],
    );
    checkpoint.status = vv_agent::CheckpointStatus::ReconciliationRequired;
    for entry in &mut checkpoint.tool_journal {
        entry.state = OperationState::Ambiguous;
    }
    checkpoint
        .validate()
        .expect("multi-entry reconciliation checkpoint");
    let store = InMemoryCheckpointStore::new();
    store
        .create_checkpoint(checkpoint)
        .expect("create multi-entry checkpoint");
    let claimed = store
        .claim_checkpoint(
            key,
            1,
            "claim-recovery-multi",
            10_000,
            1,
            ClaimMode::Recovery,
        )
        .expect("recovery claim")
        .expect("claimed checkpoint");
    let handle_a = DeferredToolHandle::new(key, "op_accept_a", 1, digest_a).expect("handle a");
    let handle_b = DeferredToolHandle::new(key, "op_accept_b", 1, digest_b).expect("handle b");
    let decisions = vec![
        AcceptDeferredDecision::new(handle_a.clone()),
        AcceptDeferredDecision::new(handle_b.clone()),
    ];
    let admission = store
        .accept_deferred_batch(key, claimed.revision, "claim-recovery-multi", 1, &decisions)
        .expect("multi-handle acceptance");
    assert_eq!(admission.handles, vec![handle_a.clone(), handle_b.clone()]);
    assert_eq!(
        admission.checkpoint.status,
        vv_agent::CheckpointStatus::Deferred
    );
    let revision = admission.checkpoint.revision;
    let outbox = admission.checkpoint.event_outbox.clone();
    assert_eq!(
        outbox.len(),
        4,
        "each accepted handle emits two durable events"
    );

    let replay = store
        .accept_deferred_batch(key, revision, "replay-without-claim", 1, &decisions)
        .expect("exact multi-handle replay");
    assert_eq!(replay.checkpoint.revision, revision);
    assert_eq!(replay.checkpoint.event_outbox, outbox);

    for (name, attempted) in [
        (
            "subset",
            vec![AcceptDeferredDecision::new(handle_a.clone())],
        ),
        (
            "duplicate",
            vec![
                AcceptDeferredDecision::new(handle_a.clone()),
                AcceptDeferredDecision::new(handle_a.clone()),
            ],
        ),
        (
            "wrong handle",
            vec![AcceptDeferredDecision::new(
                DeferredToolHandle::new(key, "op_missing", 1, "c".repeat(64))
                    .expect("wrong handle"),
            )],
        ),
    ] {
        let error = store
            .accept_deferred_batch(key, revision, "no-claim", 1, &attempted)
            .expect_err(name);
        assert_eq!(error.code(), "reconciliation_required", "{name}");
        let retained = store
            .load_checkpoint(key)
            .expect("load retained checkpoint")
            .expect("retained checkpoint");
        assert_eq!(
            retained.revision, revision,
            "{name} must not revise checkpoint"
        );
        assert_eq!(
            retained.event_outbox, outbox,
            "{name} must not append outbox"
        );
    }
}
