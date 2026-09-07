use super::*;

#[test]
fn distributed_pending_outbox_delivers_non_tool_events_in_order() {
    let store = Arc::new(InMemoryCheckpointStore::new());
    let event_store = Arc::new(InMemoryRunEventStore::default());
    let checkpoint = minimal_checkpoint(
        "pending-mixed-outbox",
        "task-pending-mixed",
        "run-pending-mixed",
        "trace-pending-mixed",
    );
    store.create_checkpoint(checkpoint.clone()).unwrap();
    let mut expected = checkpoint.clone();
    attach_succeeded_model_accounting(&mut expected, &journal_entry("model_succeeded"));
    let expected_ids = expected
        .event_outbox
        .iter()
        .map(|entry| entry.event_id.clone())
        .collect::<Vec<_>>();
    let pending_outbox = expected.event_outbox;
    let executor = TestExecutor::new(move |envelope, _, progress| {
        let mut committed = progress.checkpoint().clone();
        committed.event_outbox.extend(pending_outbox.clone());
        committed.cycle_index = u64::from(envelope.cycle_index);
        Ok(DistributedCycleOutcome::Continue(committed))
    });
    let registry = registry_with_store(store.clone(), Some(event_store.clone()));
    let dispatch = DistributedCycleWorker::new(registry)
        .with_checkpoint_executor(Arc::new(executor))
        .run_cycle(envelope(&checkpoint, 1, ClaimMode::Continue, 1_000, true))
        .unwrap();

    assert!(matches!(dispatch, CycleDispatchResult::Committed { .. }));
    let persisted = store
        .load_checkpoint(&checkpoint.checkpoint_key)
        .unwrap()
        .unwrap();
    assert_eq!(expected_ids.len(), 2);
    assert!(persisted.event_outbox.is_empty());
    let replayed = event_store
        .replay(RunEventReplayQuery::run(&checkpoint.root_run_id))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        replayed
            .iter()
            .map(|event| event.event_id().as_str())
            .map(str::to_string)
            .collect::<Vec<_>>(),
        expected_ids
    );
}
