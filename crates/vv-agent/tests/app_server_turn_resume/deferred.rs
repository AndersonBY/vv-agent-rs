use super::*;

#[tokio::test]
async fn resumed_run_uses_same_turn_and_emits_reconciliation_sequence() {
    let thread_id = "thread_1";
    let turn_id = "turn_1";
    let run_id = "run-resume-1";
    let checkpoint_key = "tenant-7/run-42";
    let checkpoint = checkpoint(
        checkpoint_key,
        CheckpointSummaryStatus::ReconciliationRequired,
        false,
    );
    let interruption = interruption();
    let response = running_response(thread_id, turn_id, run_id, None);
    let completion = completion(
        thread_id,
        turn_id,
        run_id,
        TurnStatus::Interrupted,
        Some(checkpoint.clone()),
        Some(interruption.clone()),
    );
    let outcome = DurableTurnResumeOutcome::Started {
        response,
        completion: Box::pin(async move { Ok(completion) }),
    };
    let mut harness = harness(outcome);
    initialize(&mut harness).await;

    send_resume(&mut harness, checkpoint_key).await;
    let response = next_message(&mut harness.outgoing).await;
    let JsonRpcMessage::Response(response) = response else {
        panic!("turn/resume response must be first");
    };
    let response: TurnResumeResponse =
        serde_json::from_value(response.result).expect("resume response");
    assert_eq!(response.status, TurnStatus::Running);
    assert_eq!(response.thread_id, harness.thread_id);
    assert_eq!(response.turn_id, harness.turn_id);

    let running = next_notification(&mut harness.outgoing).await;
    let started = next_notification(&mut harness.outgoing).await;
    let idle = next_notification(&mut harness.outgoing).await;
    let completed = next_notification(&mut harness.outgoing).await;
    assert!(matches!(
        running,
        ServerNotification::ThreadStatusChanged(ref params)
            if params.status == ThreadStatus::Running
    ));
    assert!(matches!(
        started,
        ServerNotification::TurnStarted(ref params)
            if params.thread_id == harness.thread_id
                && params.turn_id == harness.turn_id
                && params.run_id.as_deref() == Some(run_id)
                && params.status == Some(TurnStatus::Running)
    ));
    assert!(matches!(
        idle,
        ServerNotification::ThreadStatusChanged(ref params)
            if params.status == ThreadStatus::Idle
    ));
    let ServerNotification::TurnCompleted(completed) = completed else {
        panic!("expected turn/completed");
    };
    assert_eq!(completed.status, TurnStatus::Interrupted);
    assert_eq!(completed.completion_reason, None);
    assert_eq!(completed.error, None);
    assert_eq!(completed.checkpoint, Some(checkpoint));
    assert_eq!(completed.interruption, Some(interruption));

    let stored = harness
        .store
        .get_turn(&harness.thread_id, &harness.turn_id)
        .expect("stored turn")
        .expect("same turn");
    assert_eq!(stored.status, TurnStatus::Interrupted);
    assert_eq!(
        harness
            .store
            .list_turns(&harness.thread_id)
            .expect("turns")
            .len(),
        1
    );
    let completion_json = serde_json::to_value(&completed).expect("completion");
    for field in ["tokenUsage", "budgetUsage", "budgetExhaustion"] {
        assert!(
            !completion_json
                .as_object()
                .expect("completion object")
                .contains_key(field),
            "reconciliation projections must omit {field}"
        );
        assert!(
            !stored.result.contains_key(field),
            "stored reconciliation result must omit {field}"
        );
    }
    assert_sensitive_fields_absent(&completion_json);
    assert_sensitive_fields_absent(&serde_json::to_value(&stored.result).expect("stored result"));
    assert_requests(&harness, checkpoint_key, 1);
}
