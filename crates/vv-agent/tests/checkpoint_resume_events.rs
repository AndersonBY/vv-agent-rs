use vv_agent::{RunEvent, RunEventPayload};

const FIXTURE: &str = include_str!("fixtures/parity/resume_events.jsonl");

#[test]
fn resume_event_fixture_round_trips_typed_payloads() {
    let expected_types = [
        "checkpoint_created",
        "checkpoint_resumed",
        "operation_replayed",
        "operation_ambiguous",
        "reconciliation_required",
        "operation_ambiguous",
        "model_retry_duplicate_risk",
        "operation_ambiguous",
        "reconciliation_required",
        "reconciliation_resolved",
        "tool_call_deferred",
        "tool_call_completed",
        "tool_call_deferred",
        "tool_call_completed",
        "reconciliation_resolved",
        "tool_call_deferred",
        "cycle_aborted",
    ];

    let lines = FIXTURE.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), expected_types.len());

    for (line, expected_type) in lines.into_iter().zip(expected_types) {
        let expected: serde_json::Value = serde_json::from_str(line).expect("fixture JSON");
        let event: RunEvent = serde_json::from_str(line).expect("typed resume event");
        let encoded = serde_json::to_value(&event).expect("serialize resume event");

        assert_eq!(expected["type"], expected_type);
        assert_eq!(encoded, expected);
        assert!(matches!(
            event.payload(),
            RunEventPayload::CheckpointCreated { .. }
                | RunEventPayload::CheckpointResumed { .. }
                | RunEventPayload::OperationReplayed { .. }
                | RunEventPayload::OperationAmbiguous { .. }
                | RunEventPayload::ReconciliationRequired { .. }
                | RunEventPayload::ModelRetryDuplicateRisk { .. }
                | RunEventPayload::ReconciliationResolved { .. }
                | RunEventPayload::ToolCallDeferred { .. }
                | RunEventPayload::ToolCallCompleted { .. }
                | RunEventPayload::CycleAborted { .. }
        ));
    }
}

#[test]
fn tool_call_completed_preserves_checkpoint_key_and_rejects_unknown_fields() {
    let line = FIXTURE.lines().nth(11).expect("completed deferred event");
    let expected: serde_json::Value = serde_json::from_str(line).expect("fixture JSON");
    let event: RunEvent = serde_json::from_str(line).expect("typed completed event");
    let encoded = serde_json::to_value(event).expect("serialize completed event");
    assert_eq!(encoded["checkpoint_key"], expected["checkpoint_key"]);

    let mut unknown = expected;
    unknown["future_field"] = serde_json::json!(true);
    let error = serde_json::from_value::<RunEvent>(unknown)
        .expect_err("unknown tool completion field must be rejected");
    assert!(error.to_string().contains("unknown fields: future_field"));
}

#[test]
fn resume_events_reject_invalid_operation_boundaries() {
    let mut ambiguous: serde_json::Value =
        serde_json::from_str(FIXTURE.lines().nth(3).expect("ambiguous event"))
            .expect("fixture JSON");
    ambiguous["idempotency_support"] = serde_json::Value::Null;
    let error = serde_json::from_value::<RunEvent>(ambiguous)
        .expect_err("ambiguous tool needs idempotency support");
    assert!(error.to_string().contains("idempotency_support"));

    let mut replay: serde_json::Value =
        serde_json::from_str(FIXTURE.lines().nth(2).expect("replay event")).expect("fixture JSON");
    replay["receipt_state"] = serde_json::json!("started");
    let error = serde_json::from_value::<RunEvent>(replay).expect_err("started is not a receipt");
    assert!(error.to_string().contains("receipt_state"));
}
