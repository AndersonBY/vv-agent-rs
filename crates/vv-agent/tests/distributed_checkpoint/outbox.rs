use super::*;

struct ObserveCommitTranscriptStore {
    inner: InMemoryRunEventStore,
    checkpoints: Arc<InMemoryCheckpointStore>,
    initial: vv_agent::Checkpoint,
}

impl vv_agent::IdempotentRunEventStore for ObserveCommitTranscriptStore {
    fn append_once(
        &self,
        event_id: &str,
        payload_digest: &str,
        event: &Value,
    ) -> vv_agent::checkpoint::CheckpointResult<vv_agent::AppendOnceResult> {
        let checkpoint = self
            .checkpoints
            .load_checkpoint(&self.initial.checkpoint_key)
            .unwrap()
            .unwrap();
        assert_eq!(checkpoint.cycle_index, self.initial.cycle_index);
        assert_eq!(checkpoint.messages, self.initial.messages);
        assert_eq!(checkpoint.cycles, self.initial.cycles);
        assert_eq!(checkpoint.shared_state, self.initial.shared_state);
        vv_agent::IdempotentRunEventStore::append_once(&self.inner, event_id, payload_digest, event)
    }
}

#[test]
fn distributed_pending_outbox_delivers_non_tool_events_in_order() {
    let store = Arc::new(InMemoryCheckpointStore::new());
    let checkpoint = minimal_checkpoint(
        "pending-mixed-outbox",
        "task-pending-mixed",
        "run-pending-mixed",
        "trace-pending-mixed",
    );
    let event_store = Arc::new(ObserveCommitTranscriptStore {
        inner: InMemoryRunEventStore::default(),
        checkpoints: store.clone(),
        initial: checkpoint.clone(),
    });
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
        committed
            .messages
            .push(Message::assistant("committed answer"));
        committed.cycles.push(vv_agent::CycleRecord::from_response(
            envelope.cycle_index,
            &LLMResponse::new("committed answer"),
            Vec::new(),
        ));
        committed
            .shared_state
            .insert("committed".to_string(), json!(true));
        Ok(DistributedCycleOutcome::Continue(committed))
    });
    let registry = registry_with_store(store.clone(), None);
    registry.register_checkpoint_event_store(event_store_ref(), event_store.clone());
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
    assert_eq!(persisted.cycle_index, 1);
    assert_eq!(persisted.cycles.len(), 1);
    assert_eq!(
        persisted.messages.last(),
        Some(&Message::assistant("committed answer"))
    );
    assert_eq!(persisted.shared_state["committed"], json!(true));
    let replayed = event_store
        .inner
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
