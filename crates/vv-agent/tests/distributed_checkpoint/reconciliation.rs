use super::*;

struct ReplayToolReconciliationProvider;

fn delivered_tool_completions(
    event_store: &InMemoryRunEventStore,
    run_id: &str,
    tool_call_id: &str,
) -> Vec<RunEvent> {
    event_store
        .replay(RunEventReplayQuery::run(run_id))
        .expect("replay durable events")
        .filter_map(Result::ok)
        .filter(|event| {
            matches!(
                &event.payload,
                RunEventPayload::ToolCallCompleted {
                    tool_call_id: completed_call_id,
                    ..
                } if completed_call_id == tool_call_id
            )
        })
        .collect()
}

impl ReconciliationProvider for ReplayToolReconciliationProvider {
    fn reconcile(
        &self,
        _observation: &vv_agent::ResumeObservation,
    ) -> vv_agent::checkpoint::CheckpointResult<ReconciliationDecision> {
        Ok(ReconciliationDecision::replay_result(
            ToolExecutionResult::success("call_1", "replayed").to_dict(),
        ))
    }
}

#[test]
fn distributed_surface_to_model_recovery_materializes_one_unknown_receipt() {
    let store = Arc::new(InMemoryCheckpointStore::new());
    let event_store = Arc::new(InMemoryRunEventStore::default());
    let mut checkpoint = minimal_checkpoint(
        "distributed-surface-to-model",
        "task-distributed-surface",
        "run-distributed-surface",
        "trace-distributed-surface",
    );
    checkpoint.run_definition["checkpoint_policy"]["ambiguous_tool_policy"] =
        json!("surface_to_model");
    checkpoint.run_definition_digest =
        vv_agent::run_definition_digest(&checkpoint.run_definition).unwrap();
    checkpoint.tool_journal.push(journal_entry("tool_started"));
    let checkpoint = create_claimed_snapshot(store.as_ref(), checkpoint, "expired-owner", 1, 0);
    let observed_journal = Arc::new(Mutex::new(Vec::new()));
    let observed_journal_for_executor = observed_journal.clone();
    let executor = TestExecutor::new(move |envelope, _, progress| {
        let current = progress.checkpoint().clone();
        observed_journal_for_executor
            .lock()
            .expect("observed journal")
            .extend(current.tool_journal.clone());
        let mut committed = current;
        committed.cycle_index = u64::from(envelope.cycle_index);
        Ok(DistributedCycleOutcome::Continue(committed))
    });
    let registry = registry_with_store(store.clone(), Some(event_store.clone()));
    let worker = DistributedCycleWorker::new(registry).with_checkpoint_executor(Arc::new(executor));
    let mut envelope = envelope(&checkpoint, 1, ClaimMode::Continue, 1_000, true);
    envelope.checkpoint_config.ambiguous_tool_policy = AmbiguousToolPolicy::SurfaceToModel;

    let dispatch = worker
        .run_cycle_with_delivery(envelope, DistributedDeliveryMetadata::redelivery(2))
        .expect("distributed recovery");
    assert!(matches!(
        dispatch,
        CycleDispatchResult::Committed {
            committed_cycle: 1,
            ..
        }
    ));

    let journal = observed_journal
        .lock()
        .expect("observed journal")
        .first()
        .cloned()
        .expect("closed tool journal");
    assert_eq!(journal.state, OperationState::Failed);
    assert!(journal.identity_key.is_some());
    assert!(journal.result_digest.is_some());
    assert_eq!(
        journal.error.as_ref().map(|error| error.code.as_str()),
        Some("tool_outcome_unknown")
    );
    let persisted = store
        .load_checkpoint("distributed-surface-to-model")
        .expect("load persisted checkpoint")
        .expect("persisted checkpoint");
    let pending_completions = persisted
        .event_outbox
        .iter()
        .filter(|entry| {
            entry.state == "pending"
                && entry.event["type"] == "tool_call_completed"
                && entry.event["tool_call_id"] == "call_1"
        })
        .collect::<Vec<_>>();
    assert!(pending_completions.is_empty());
    let completions =
        delivered_tool_completions(event_store.as_ref(), "run-distributed-surface", "call_1");
    assert_eq!(completions.len(), 1);
    assert!(matches!(
        &completions[0].payload,
        RunEventPayload::ToolCallCompleted {
            error_code: Some(error_code),
            ..
        } if error_code == "tool_outcome_unknown"
    ));
}

#[test]
fn distributed_replay_success_materializes_one_canonical_receipt() {
    let store = Arc::new(InMemoryCheckpointStore::new());
    let event_store = Arc::new(InMemoryRunEventStore::default());
    let provider_ref = CapabilityRef::new("reconciliation.replay", "1").unwrap();
    let mut checkpoint = minimal_checkpoint(
        "distributed-replay-success",
        "task-distributed-replay",
        "run-distributed-replay",
        "trace-distributed-replay",
    );
    checkpoint.run_definition["capability_refs"]["reconciliation_provider"] =
        provider_ref.to_dict();
    checkpoint.run_definition_digest =
        vv_agent::run_definition_digest(&checkpoint.run_definition).unwrap();
    checkpoint.tool_journal.push(journal_entry("tool_started"));
    let checkpoint = create_claimed_snapshot(store.as_ref(), checkpoint, "expired-owner", 1, 0);
    let observed_journal = Arc::new(Mutex::new(Vec::new()));
    let observed_journal_for_executor = observed_journal.clone();
    let executor = TestExecutor::new(move |envelope, _, progress| {
        let current = progress.checkpoint().clone();
        observed_journal_for_executor
            .lock()
            .expect("observed journal")
            .extend(current.tool_journal.clone());
        let mut committed = current;
        committed.cycle_index = u64::from(envelope.cycle_index);
        Ok(DistributedCycleOutcome::Continue(committed))
    });
    let registry = registry_with_store(store.clone(), Some(event_store.clone()));
    registry.register_reconciliation_provider(
        provider_ref.clone(),
        Arc::new(ReplayToolReconciliationProvider),
    );
    let worker = DistributedCycleWorker::new(registry).with_checkpoint_executor(Arc::new(executor));
    let mut envelope = envelope(&checkpoint, 1, ClaimMode::Continue, 1_000, true);
    envelope.recipe.capabilities.reconciliation_provider_ref = Some(provider_ref);

    let dispatch = worker
        .run_cycle_with_delivery(envelope, DistributedDeliveryMetadata::redelivery(2))
        .expect("distributed replay recovery");
    assert!(matches!(
        dispatch,
        CycleDispatchResult::Committed {
            committed_cycle: 1,
            ..
        }
    ));

    let journal = observed_journal
        .lock()
        .expect("observed journal")
        .first()
        .cloned()
        .expect("closed tool journal");
    assert_eq!(journal.state, OperationState::Succeeded);
    assert!(journal.identity_key.is_some());
    assert!(journal.result_digest.is_some());
    assert_eq!(
        journal.result.as_ref().expect("replayed result")["content"],
        "replayed"
    );
    let persisted = store
        .load_checkpoint("distributed-replay-success")
        .expect("load persisted checkpoint")
        .expect("persisted checkpoint");
    let pending_completions = persisted
        .event_outbox
        .iter()
        .filter(|entry| {
            entry.state == "pending"
                && entry.event["type"] == "tool_call_completed"
                && entry.event["tool_call_id"] == "call_1"
        })
        .collect::<Vec<_>>();
    assert!(pending_completions.is_empty());
    let completions =
        delivered_tool_completions(event_store.as_ref(), "run-distributed-replay", "call_1");
    assert_eq!(completions.len(), 1);
    assert!(matches!(
        &completions[0].payload,
        RunEventPayload::ToolCallCompleted {
            status: vv_agent::ToolStatus::Success,
            ..
        }
    ));
}

#[test]
fn definition_and_resume_attempt_mismatch_fail_before_claim() {
    let store = Arc::new(InMemoryCheckpointStore::new());
    let checkpoint = minimal_checkpoint(
        "identity-mismatch",
        "task-identity",
        "run-identity",
        "trace-identity",
    );
    store.create_checkpoint(checkpoint.clone()).unwrap();
    let registry = registry_with_store(store.clone(), None);
    let worker = DistributedCycleWorker::new(registry);

    let mut wrong_definition = envelope(&checkpoint, 1, ClaimMode::Continue, 1_000, false);
    wrong_definition.run_definition_digest = "d".repeat(64);
    assert_eq!(
        worker.run_cycle(wrong_definition).unwrap_err(),
        "checkpoint_definition_mismatch"
    );

    let mut wrong_task = envelope(&checkpoint, 1, ClaimMode::Continue, 1_000, false);
    wrong_task.task.prompt_bundle = PromptBundle::from_instruction_text("tampered prompt")
        .expect("valid tampered prompt bundle");
    assert!(worker
        .run_cycle(wrong_task)
        .unwrap_err()
        .contains("checkpoint_definition_mismatch"));

    let mut wrong_budget = envelope(&checkpoint, 1, ClaimMode::Continue, 1_000, false);
    wrong_budget.budget_limits = Some(
        RunBudgetLimits::builder()
            .max_total_tokens(10)
            .build()
            .unwrap(),
    );
    assert!(worker
        .run_cycle(wrong_budget)
        .unwrap_err()
        .contains("checkpoint_definition_mismatch"));

    let mut wrong_policy = envelope(&checkpoint, 1, ClaimMode::Continue, 1_000, false);
    wrong_policy.recipe.capabilities.tool_policy.allowed_tools = Some(Vec::new());
    assert!(worker
        .run_cycle(wrong_policy)
        .unwrap_err()
        .contains("checkpoint_definition_mismatch"));

    let mut wrong_attempt = envelope(&checkpoint, 1, ClaimMode::Continue, 1_000, false);
    wrong_attempt.resume_attempt = 2;
    assert_eq!(
        worker.run_cycle(wrong_attempt).unwrap_err(),
        "checkpoint_resume_attempt_mismatch"
    );
    let persisted = store.load_checkpoint("identity-mismatch").unwrap().unwrap();
    assert_eq!(persisted.revision, 0);
    assert!(persisted.claim_token.is_none());
}
