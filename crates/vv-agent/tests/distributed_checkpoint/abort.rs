use super::*;

struct AbortReconciliationProvider;

impl ReconciliationProvider for AbortReconciliationProvider {
    fn reconcile(
        &self,
        _observation: &vv_agent::ResumeObservation,
    ) -> vv_agent::checkpoint::CheckpointResult<ReconciliationDecision> {
        Ok(ReconciliationDecision::abort(ReconciliationError::new(
            "external_outcome_unknown",
            "the external outcome is unknown",
            false,
        )))
    }
}

#[test]
fn distributed_abort_uses_typed_error_and_terminal_finalizer_closes_unknown_tool() {
    let store = Arc::new(InMemoryCheckpointStore::new());
    let mut checkpoint = minimal_checkpoint(
        "distributed-abort",
        "task-distributed-abort",
        "run-distributed-abort",
        "trace-distributed-abort",
    );
    checkpoint.tool_journal.push(journal_entry("tool_started"));

    let provider_ref = CapabilityRef::new("reconciliation.abort", "1").unwrap();
    checkpoint.run_definition["capability_refs"]["reconciliation_provider"] =
        provider_ref.to_dict();
    checkpoint.run_definition_digest =
        vv_agent::run_definition_digest(&checkpoint.run_definition).unwrap();
    checkpoint = create_claimed_snapshot(store.as_ref(), checkpoint, "expired-owner", 1, 0);
    let registry = registry_with_store(store.clone(), None);
    registry.register_reconciliation_provider(
        provider_ref.clone(),
        Arc::new(AbortReconciliationProvider),
    );
    let mut envelope = envelope(&checkpoint, 1, ClaimMode::Continue, 1_000, false);
    envelope.recipe.capabilities.reconciliation_provider_ref = Some(provider_ref);
    let executor =
        TestExecutor::new(|_, _, _| unreachable!("recovery abort must not execute a new cycle"));
    let dispatch = DistributedCycleWorker::new(registry)
        .with_checkpoint_executor(Arc::new(executor))
        .run_cycle_with_delivery(envelope, DistributedDeliveryMetadata::redelivery(2))
        .unwrap();
    let CycleDispatchResult::TerminalCandidate { result, .. } = dispatch else {
        panic!("expected terminal candidate");
    };
    assert_eq!(
        result.error.as_ref().map(|error| error.code.as_str()),
        Some("operator_abort_with_unknown_outcome")
    );
    assert_eq!(
        result.error.as_ref().map(|error| error.message.as_str()),
        Some("Operator accepted that the external outcome is unknown.")
    );
    assert!(result.error_code.is_none());

    let claimed = store.load_checkpoint("distributed-abort").unwrap().unwrap();
    let mut terminal = claimed.clone();
    terminal.status = CheckpointStatus::Failed;
    terminal.terminal_result = Some(result.to_dict());
    let claim_token = claimed.claim_token.clone().unwrap();
    assert!(store
        .finalize_claimed_checkpoint(terminal, &claim_token, claimed.revision)
        .unwrap());
    let finalized = store.load_checkpoint("distributed-abort").unwrap().unwrap();
    assert_eq!(finalized.tool_journal[0].state, OperationState::Failed);
    assert_eq!(
        finalized.tool_journal[0]
            .error
            .as_ref()
            .map(|error| error.code.as_str()),
        Some("tool_cancelled")
    );
    assert!(finalized
        .event_outbox
        .iter()
        .any(|event| event.event["type"] == "cycle_aborted" && event.event["logical_cycle"] == 1));
    assert_eq!(
        finalized.terminal_result.as_ref().unwrap()["error"]["code"],
        "operator_abort_with_unknown_outcome"
    );
}

#[test]
fn definition_validation_redacts_credentials_and_normalizes_tool_policy_sets() {
    let store = Arc::new(InMemoryCheckpointStore::new());
    let mut checkpoint = minimal_checkpoint(
        "normalized-definition",
        "task-normalized",
        "run-normalized",
        "trace-normalized",
    );
    checkpoint.run_definition["credential_slots"] =
        json!(["/model/settings/extra_headers/authorization"]);
    checkpoint.run_definition["model"]["settings"] = json!({
        "extra_headers": {
            "authorization": vv_agent::checkpoint::CREDENTIAL_REDACTED,
        },
    });
    checkpoint.run_definition["tool_policy"]["allowed_tools"] = json!(["alpha", "beta"]);
    checkpoint.run_definition["tool_policy"]["disallowed_tools"] =
        json!(["blocked-a", "blocked-b"]);
    checkpoint.run_definition_digest =
        vv_agent::run_definition_digest(&checkpoint.run_definition).unwrap();
    store.create_checkpoint(checkpoint.clone()).unwrap();

    let executor = TestExecutor::new(move |envelope, _, progress| {
        let mut committed = progress.checkpoint().clone();
        committed.cycle_index = u64::from(envelope.cycle_index);
        Ok(DistributedCycleOutcome::Continue(committed))
    });
    let registry = registry_with_store(store.clone(), None);
    let worker = DistributedCycleWorker::new(registry).with_checkpoint_executor(Arc::new(executor));
    let mut envelope = envelope(&checkpoint, 1, ClaimMode::Continue, 1_000, false);
    envelope.checkpoint_config.credential_slots =
        vec!["/model/settings/extra_headers/authorization".to_string()];
    envelope.task.model_settings = Some(
        ModelSettings::builder()
            .extra_header("Authorization", "live-secret")
            .build(),
    );
    envelope.recipe.capabilities.tool_policy.allowed_tools = Some(vec![
        "beta".to_string(),
        "alpha".to_string(),
        "alpha".to_string(),
    ]);
    envelope.recipe.capabilities.tool_policy.disallowed_tools = vec![
        "blocked-b".to_string(),
        "blocked-a".to_string(),
        "blocked-b".to_string(),
    ];

    let dispatch = worker.run_cycle(envelope).unwrap();

    assert!(matches!(
        dispatch,
        CycleDispatchResult::Committed {
            committed_cycle: 1,
            ..
        }
    ));
    let persisted = store
        .load_checkpoint("normalized-definition")
        .unwrap()
        .unwrap();
    assert_eq!(persisted.cycle_index, 1);
}
