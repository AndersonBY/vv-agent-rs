use super::*;

#[test]
fn recovery_worker_consumes_host_response_and_excludes_duplicate_execution() {
    let store = Arc::new(InMemoryCheckpointStore::new());
    let checkpoint = minimal_checkpoint(
        "worker-host-recovery-barrier",
        "task-host-recovery-barrier",
        "run-host-recovery-barrier",
        "trace-host-recovery-barrier",
    );
    let key = checkpoint.checkpoint_key.clone();
    store
        .create_checkpoint(initial_checkpoint(checkpoint))
        .expect("create initial checkpoint");
    let claimed = store
        .claim_checkpoint(&key, 1, "worker-owner", 10_000, 0, ClaimMode::Continue)
        .expect("claim checkpoint")
        .expect("claimed checkpoint");
    let request = HostInteractionRequest::new(
        "interaction-worker-host-recovery",
        1,
        "operation-worker-host-recovery",
        "tool-worker-host-recovery",
        "Choose an option.",
    )
    .expect("host interaction request");
    let admission =
        HostInteractionAdmissionContext::new(&key, claimed.revision, "worker-owner", 1, 0, 10_000)
            .expect("host interaction admission");
    let admitted = store
        .produce_host_interaction(request.clone(), &admission)
        .expect("produce host interaction");
    let command = ControllerCommand::new(
        "command-worker-host-recovery",
        ControllerHandle::new(&key, &claimed.root_run_id, &claimed.trace_id)
            .expect("controller handle"),
        claimed.resume_attempt,
        admitted.checkpoint_revision,
        ControllerCommandVariant::HostInteractionResponse {
            interaction_id: request.interaction_id.clone(),
            logical_cycle: request.logical_cycle,
            operation_id: request.operation_id.clone(),
            tool_call_id: request.tool_call_id.clone(),
            request_digest: request.request_digest.clone(),
            response: HostInteractionMessage::user("approved").expect("host response"),
        },
    )
    .expect("host response command");
    store
        .resolve_controller_command(command)
        .expect("resolve host response");
    let before = store
        .load_checkpoint(&key)
        .expect("load barrier checkpoint")
        .expect("barrier checkpoint");
    let registry = registry_with_store(store.clone(), None);
    let duplicate_worker = DistributedCycleWorker::new(registry.clone());
    let observed_store = store.clone();
    let original = envelope(&before, 1, ClaimMode::Recovery, 1_000, false);
    let duplicate_envelope = original.clone();
    let executor = TestExecutor::new(move |envelope, _, progress| {
        let owned = observed_store
            .load_checkpoint(&duplicate_envelope.checkpoint_config.key)
            .unwrap()
            .unwrap();
        assert_eq!(owned.resume_attempt, 2);
        assert_eq!(
            owned
                .messages
                .iter()
                .filter(|message| message.content == "approved")
                .count(),
            1
        );
        assert_eq!(
            duplicate_worker
                .run_cycle(duplicate_envelope.clone())
                .unwrap(),
            CycleDispatchResult::pending()
        );
        assert_eq!(
            observed_store
                .load_checkpoint(&owned.checkpoint_key)
                .unwrap(),
            Some(owned)
        );
        let mut committed = progress.checkpoint().clone();
        committed.cycle_index = u64::from(envelope.cycle_index);
        Ok(DistributedCycleOutcome::Continue(committed))
    });
    let worker = DistributedCycleWorker::new(registry).with_checkpoint_executor(Arc::new(executor));
    {
        let error = worker
            .run_cycle(envelope(&before, 1, ClaimMode::Continue, 1_000, false))
            .expect_err("ordinary worker claim must stop at host recovery barrier");
        assert!(
            error.contains("host_interaction_recovery_required"),
            "unexpected worker barrier error: {error}"
        );
        let after = store
            .load_checkpoint(&key)
            .expect("load after worker barrier")
            .expect("checkpoint after worker barrier");
        assert_eq!(after, before);
    }
    assert!(matches!(
        worker.run_cycle(original.clone()).unwrap(),
        CycleDispatchResult::Committed { .. }
    ));
    let committed = store.load_checkpoint(&key).unwrap().unwrap();
    assert!(committed.claim_token.is_none());
    assert!(matches!(
        worker.run_cycle(original).unwrap(),
        CycleDispatchResult::Committed { .. }
    ));
    assert_eq!(store.load_checkpoint(&key).unwrap(), Some(committed));
}
