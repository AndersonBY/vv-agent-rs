use super::*;

#[test]
fn worker_replays_terminal_checkpoint_and_delivers_pending_outbox_once() {
    let store = Arc::new(InMemoryCheckpointStore::new());
    let event_store = Arc::new(InMemoryRunEventStore::default());
    let checkpoint = minimal_checkpoint(
        "terminal-worker-replay",
        "task-terminal-worker-replay",
        "run-terminal-worker-replay",
        "trace-terminal-worker-replay",
    );
    store.create_checkpoint(checkpoint.clone()).unwrap();

    let mut terminal = checkpoint.clone();
    terminal.status = CheckpointStatus::Completed;
    let mut result = AgentResult::completed(Vec::new(), Vec::new(), "done");
    result.checkpoint_key = Some(checkpoint.checkpoint_key.clone());
    terminal.terminal_result = Some(result.to_dict());
    terminal.event_outbox.push(
        EventOutboxEntry::pending(
            "evt-terminal-worker-replay",
            json!({
                "version": "v5",
                "type": "run_completed",
                "event_id": "evt-terminal-worker-replay",
                "run_id": "run-terminal-worker-replay",
                "trace_id": "trace-terminal-worker-replay",
                "created_at": 1.0,
                "final_output": "done",
                "status": "completed",
                "completion_reason": "tool_finish",
                "completion_tool_name": "task_finish"
            }),
        )
        .unwrap(),
    );
    assert!(store
        .finalize_checkpoint(terminal, checkpoint.revision)
        .unwrap());

    let envelope = envelope(&checkpoint, 1, ClaimMode::Continue, 1_000, true);
    let worker = DistributedCycleWorker::new(registry_with_store(
        store.clone(),
        Some(event_store.clone()),
    ))
    .with_checkpoint_executor(Arc::new(TestExecutor::new(|_, _, _| {
        unreachable!("terminal replay must not execute the checkpoint executor")
    })));

    let first = worker.run_cycle(envelope.clone()).unwrap();
    let CycleDispatchResult::TerminalReplay {
        checkpoint_revision: first_revision,
        result: first_result,
    } = first
    else {
        panic!("expected terminal replay");
    };
    assert_eq!(first_result, result);
    let persisted = store
        .load_checkpoint(&checkpoint.checkpoint_key)
        .unwrap()
        .unwrap();
    assert!(persisted.terminal_acknowledged);
    assert!(persisted.claim_token.is_none());
    assert_eq!(persisted.event_outbox[0].state, "delivered");
    assert_eq!(persisted.revision, 3);
    assert_eq!(first_revision, persisted.revision);

    let second = worker.run_cycle(envelope).unwrap();
    let CycleDispatchResult::TerminalReplay {
        checkpoint_revision: second_revision,
        result: second_result,
    } = second
    else {
        panic!("expected terminal replay");
    };
    assert_eq!(second_result, result);
    assert_eq!(second_revision, persisted.revision);
    assert_eq!(
        event_store
            .replay(RunEventReplayQuery::run(&checkpoint.root_run_id))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store.load_checkpoint(&checkpoint.checkpoint_key).unwrap(),
        Some(persisted)
    );
}
