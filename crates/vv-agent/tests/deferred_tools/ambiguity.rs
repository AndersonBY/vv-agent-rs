use super::*;

#[test]
fn mixed_deferred_batch_classifies_ambiguous_and_definitive_errors() {
    for (index, (code, definitive)) in [
        ("tool_timeout", false),
        ("tool_execution_failed", false),
        ("tool_orchestrator_error", false),
        ("tool_execution_failed", true),
    ]
    .into_iter()
    .enumerate()
    {
        let key = format!("memory-deferred-{index}");
        let claim_token = format!("claim-{index}");
        let digest_deferred = "a".repeat(64);
        let digest_completed = "b".repeat(64);
        let checkpoint = checkpoint_with_started_tools(
            &key,
            &[
                ("op_deferred", "call_deferred", &digest_deferred),
                ("op_completed", "call_completed", &digest_completed),
            ],
        );
        let store = InMemoryCheckpointStore::new();
        store
            .create_checkpoint(checkpoint)
            .expect("create checkpoint");
        let claimed = store
            .claim_checkpoint(&key, 1, &claim_token, 10_000, 1, ClaimMode::Continue)
            .expect("claim checkpoint")
            .expect("claimed checkpoint");
        let handle = DeferredToolHandle::new(&key, "op_deferred", 1, digest_deferred)
            .expect("deferred handle");
        let mut completed = ToolExecutionResult::error(
            "call_completed",
            if definitive {
                "definitively failed"
            } else {
                "outcome is unknown"
            },
        )
        .with_error_code(code);
        if definitive {
            completed
                .metadata
                .insert("definitive_outcome".to_string(), json!(true));
        }
        let validation = vv_agent::checkpoint::validate_definitive_result(&completed);
        assert_eq!(validation.is_ok(), definitive);
        let admission = store.admit_deferred_batch(
            &key,
            claimed.revision,
            &claim_token,
            1,
            &[
                batch_entry(
                    "op_deferred",
                    "call_deferred",
                    &"a".repeat(64),
                    ToolCallOutcome::deferred(handle),
                ),
                batch_entry(
                    "op_completed",
                    "call_completed",
                    &"b".repeat(64),
                    ToolCallOutcome::completed(completed),
                ),
            ],
        );
        if definitive {
            let admission = admission.expect("definitive error admission");
            assert_eq!(
                admission.checkpoint.status,
                vv_agent::CheckpointStatus::Deferred
            );
            assert_eq!(
                admission
                    .checkpoint
                    .tool_journal
                    .iter()
                    .find(|entry| entry.operation_id == "op_completed")
                    .expect("completed journal entry")
                    .state,
                OperationState::Failed
            );
        } else {
            let error = admission.expect_err("ambiguous outcome must not enter a deferred barrier");
            assert_eq!(error.code(), "deferred_batch_result_invalid");
            assert_eq!(
                store
                    .load_checkpoint(&key)
                    .expect("load retained checkpoint")
                    .expect("retained checkpoint"),
                claimed,
                "rejected admission must not write a partial batch"
            );
        }
    }
}

fn model_entry(state: OperationState) -> OperationJournalEntry {
    let mut entry = OperationJournalEntry::model(
        "op_model_recovery",
        1,
        1,
        "b".repeat(64),
        ModelCallOperation::AgentCycle,
        "test",
        "test-model",
        "op_model_recovery:attempt:1",
    );
    entry.state = state;
    entry
}

fn append_model_started_event(checkpoint: &mut Checkpoint) {
    let event = RunEvent::model_call_started(
        &checkpoint.root_run_id,
        &checkpoint.trace_id,
        &checkpoint.task_id,
        1,
        "op_model_recovery:attempt:1",
        "op_model_recovery",
        1,
        ModelCallOperation::AgentCycle,
        "test",
        "test-model",
    );
    checkpoint.event_outbox.push(
        EventOutboxEntry::pending(
            event.event_id().as_str().to_string(),
            serde_json::to_value(event).expect("model event wire"),
        )
        .expect("model event outbox"),
    );
}

#[test]
fn admission_rejects_a_started_model_journal_without_a_write() {
    let key = "memory-admit-model-started";
    let digest = "a".repeat(64);
    let mut checkpoint =
        checkpoint_with_started_tools(key, &[("op_tool_started", "call_tool_started", &digest)]);
    checkpoint.model_call_journal = vec![model_entry(OperationState::Started)];
    append_model_started_event(&mut checkpoint);
    checkpoint.validate().expect("started model checkpoint");

    let store = InMemoryCheckpointStore::new();
    store
        .create_checkpoint(checkpoint)
        .expect("create checkpoint");
    let claimed = store
        .claim_checkpoint(key, 1, "claim", 10_000, 1, ClaimMode::Continue)
        .expect("claim checkpoint")
        .expect("claimed checkpoint");
    let handle = DeferredToolHandle::new(key, "op_tool_started", 1, digest).expect("handle");
    let before = store
        .load_checkpoint(key)
        .expect("load before admission")
        .expect("checkpoint before admission");
    let error = store
        .admit_deferred_batch(
            key,
            claimed.revision,
            "claim",
            1,
            &[batch_entry(
                "op_tool_started",
                "call_tool_started",
                &"a".repeat(64),
                ToolCallOutcome::deferred(handle),
            )],
        )
        .expect_err("started model journal must block admission");
    assert_eq!(error.code(), "deferred_batch_incomplete");
    assert_eq!(
        store
            .load_checkpoint(key)
            .expect("load after admission")
            .expect("checkpoint after admission"),
        before,
        "rejected admission must not write a partial batch"
    );
}

#[test]
fn acceptance_rejects_planned_or_started_model_journal_without_a_write() {
    for (index, state) in [OperationState::Planned, OperationState::Started]
        .into_iter()
        .enumerate()
    {
        let key = format!("memory-accept-model-state-{index}");
        let digest = "a".repeat(64);
        let mut checkpoint = checkpoint_with_started_tools(
            &key,
            &[("op_tool_recovery", "call_tool_recovery", &digest)],
        );
        checkpoint.tool_journal[0].state = OperationState::Ambiguous;
        checkpoint.model_call_journal = vec![model_entry(state)];
        if state == OperationState::Started {
            append_model_started_event(&mut checkpoint);
        }
        checkpoint.status = vv_agent::CheckpointStatus::ReconciliationRequired;
        checkpoint.validate().expect("model state checkpoint");

        let store = InMemoryCheckpointStore::new();
        store
            .create_checkpoint(checkpoint)
            .expect("create checkpoint");
        let claimed = store
            .claim_checkpoint(&key, 1, "claim-recovery", 10_000, 1, ClaimMode::Recovery)
            .expect("recovery claim")
            .expect("claimed checkpoint");
        let handle = DeferredToolHandle::new(&key, "op_tool_recovery", 1, digest).expect("handle");
        let before = store
            .load_checkpoint(&key)
            .expect("load before acceptance")
            .expect("checkpoint before acceptance");
        let error = store
            .accept_deferred_batch(
                &key,
                claimed.revision,
                "claim-recovery",
                1,
                &[AcceptDeferredDecision::new(handle)],
            )
            .expect_err("unresolved model journal must block acceptance");
        assert_eq!(error.code(), "reconciliation_required");
        assert_eq!(
            store
                .load_checkpoint(&key)
                .expect("load after acceptance")
                .expect("checkpoint after acceptance"),
            before,
            "rejected acceptance must not write a partial batch"
        );
    }
}
