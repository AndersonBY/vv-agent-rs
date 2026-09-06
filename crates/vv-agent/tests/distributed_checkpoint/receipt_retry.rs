use super::*;

#[test]
fn idempotent_retry_reuses_key_and_committed_cycle_absorbs_stale_redelivery() {
    let store = Arc::new(InMemoryCheckpointStore::new());
    let mut checkpoint = minimal_checkpoint(
        "idempotent-retry",
        "task-idempotent",
        "run-idempotent",
        "trace-idempotent",
    );
    let started = journal_entry("tool_started");
    let original_key = started.idempotency_key.clone();
    checkpoint.tool_journal.push(started);
    checkpoint.run_definition["checkpoint_policy"]["ambiguous_tool_policy"] =
        json!("retry_idempotent_only");
    checkpoint.run_definition_digest =
        vv_agent::run_definition_digest(&checkpoint.run_definition).unwrap();
    checkpoint =
        create_claimed_snapshot(store.as_ref(), checkpoint, "expired-idempotent-owner", 1, 0);
    let external_calls = Arc::new(AtomicUsize::new(0));
    let external_calls_for_executor = external_calls.clone();
    let executor = TestExecutor::new(move |envelope, _, progress| {
        let durable = &progress.checkpoint().tool_journal[0];
        assert_eq!(durable.state, OperationState::Planned);
        assert_eq!(durable.attempt, 2);
        assert_eq!(durable.idempotency_key, original_key);

        let mut started = progress.checkpoint().clone();
        started.tool_journal[0]
            .transition_to(OperationState::Started)
            .map_err(|error| error.to_string())?;
        progress.persist(started)?;
        external_calls_for_executor.fetch_add(1, Ordering::SeqCst);

        let entry = progress.checkpoint().tool_journal[0].clone();
        let recorded = progress.record_tool_receipt(
            &entry.operation_id,
            entry.attempt,
            entry.tool_call_id.as_deref().expect("tool call id"),
            &entry.request_digest,
            vv_agent::ToolExecutionResult::success(
                entry.tool_call_id.as_deref().expect("tool call id"),
                "receipt-1",
            ),
        )?;
        assert!(recorded);

        let mut committed = progress.checkpoint().clone();
        committed.cycle_index = u64::from(envelope.cycle_index);
        Ok(DistributedCycleOutcome::Continue(committed))
    });
    let registry = registry_with_store(store.clone(), None);
    let worker = DistributedCycleWorker::new(registry).with_checkpoint_executor(Arc::new(executor));
    let mut stale_envelope = envelope(&checkpoint, 1, ClaimMode::Continue, 1_000, false);
    stale_envelope.checkpoint_config.ambiguous_tool_policy =
        AmbiguousToolPolicy::RetryIdempotentOnly;

    let first = worker
        .run_cycle_with_delivery(
            stale_envelope.clone(),
            DistributedDeliveryMetadata::redelivery(2),
        )
        .unwrap();
    assert!(matches!(first, CycleDispatchResult::Committed { .. }));
    assert_eq!(external_calls.load(Ordering::SeqCst), 1);

    let stale_redelivery = worker
        .run_cycle_with_delivery(stale_envelope, DistributedDeliveryMetadata::redelivery(3))
        .unwrap();
    assert!(matches!(
        stale_redelivery,
        CycleDispatchResult::Committed { .. }
    ));
    assert_eq!(external_calls.load(Ordering::SeqCst), 1);
    let persisted = store.load_checkpoint("idempotent-retry").unwrap().unwrap();
    assert_eq!(persisted.cycle_index, 1);
    assert_eq!(persisted.resume_attempt, 2);
    assert!(persisted.tool_journal.is_empty());
    assert!(persisted.event_outbox.is_empty());
}
