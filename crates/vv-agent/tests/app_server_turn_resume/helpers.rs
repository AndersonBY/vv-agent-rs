fn running_response(
    thread_id: &str,
    turn_id: &str,
    run_id: &str,
    checkpoint: Option<CheckpointSummary>,
) -> TurnResumeResponse {
    TurnResumeResponse {
        thread_id: thread_id.to_string(),
        turn_id: turn_id.to_string(),
        run_id: run_id.to_string(),
        status: TurnStatus::Running,
        final_output: None,
        completion_reason: None,
        completion_tool_name: None,
        partial_output: None,
        checkpoint,
        interruption: None,
        error: None,
    }
}

fn checkpoint(
    key: &str,
    status: CheckpointSummaryStatus,
    terminal_acknowledged: bool,
) -> CheckpointSummary {
    CheckpointSummary {
        key: key.to_string(),
        resume_attempt: 2,
        cycle_index: if terminal_acknowledged { 2 } else { 1 },
        status,
        terminal_acknowledged,
    }
}

fn interruption() -> InterruptionSummary {
    InterruptionSummary {
        reason: "resume_requires_reconciliation".to_string(),
        operation_id: "op_tool_cycle_2_call_2".to_string(),
        operation_kind: InterruptionOperationKind::Tool,
        cycle_index: 2,
        risk: "unknown_tool_side_effect".to_string(),
        idempotency_support: InterruptionIdempotencySupport::Unknown,
    }
}

fn completion(
    thread_id: &str,
    turn_id: &str,
    run_id: &str,
    status: TurnStatus,
    checkpoint: Option<CheckpointSummary>,
    interruption: Option<InterruptionSummary>,
) -> TurnCompletedParams {
    TurnCompletedParams {
        thread_id: thread_id.to_string(),
        turn_id: turn_id.to_string(),
        run_id: Some(run_id.to_string()),
        status,
        final_output: None,
        completion_reason: None,
        completion_tool_name: None,
        partial_output: None,
        error: None,
        token_usage: None,
        budget_usage: None,
        budget_exhaustion: None,
        checkpoint,
        interruption,
    }
}

async fn initialize(harness: &mut Harness) {
    initialize_processor(
        &mut harness.processor,
        &mut harness.outgoing,
        ConnectionId::new(1),
    )
    .await;
}

async fn initialize_processor(
    processor: &mut MessageProcessor,
    outgoing: &mut mpsc::Receiver<OutgoingEnvelope>,
    connection_id: ConnectionId,
) {
    processor
        .process_message(
            connection_id,
            request(
                1,
                "initialize",
                json!({"clientInfo": {"name": "turn-resume-test"}}),
            ),
        )
        .await;
    let JsonRpcMessage::Response(_) = next_message(outgoing).await else {
        panic!("initialize response");
    };
    processor
        .process_message(
            connection_id,
            JsonRpcMessage::Notification(JsonRpcNotification {
                method: "initialized".to_string(),
                params: None,
            }),
        )
        .await;
}

async fn send_resume(harness: &mut Harness, checkpoint_key: &str) {
    harness
        .processor
        .process_message(
            ConnectionId::new(1),
            request(
                2,
                "turn/resume",
                json!({
                    "threadId": harness.thread_id,
                    "turnId": harness.turn_id,
                    "checkpointKey": checkpoint_key,
                }),
            ),
        )
        .await;
}

fn request(id: i64, method: &str, params: Value) -> JsonRpcMessage {
    JsonRpcMessage::Request(JsonRpcRequest {
        id: RequestId::Integer(id),
        method: method.to_string(),
        params: Some(params),
    })
}

async fn next_message(outgoing: &mut mpsc::Receiver<OutgoingEnvelope>) -> JsonRpcMessage {
    tokio::time::timeout(Duration::from_secs(1), outgoing.recv())
        .await
        .expect("outgoing message timeout")
        .expect("outgoing channel")
        .message
}

async fn next_notification(outgoing: &mut mpsc::Receiver<OutgoingEnvelope>) -> ServerNotification {
    let JsonRpcMessage::Notification(notification) = next_message(outgoing).await else {
        panic!("expected notification");
    };
    serde_json::from_value(json!({
        "method": notification.method,
        "params": notification.params,
    }))
    .expect("typed notification")
}

async fn assert_no_message(outgoing: &mut mpsc::Receiver<OutgoingEnvelope>) {
    assert!(
        tokio::time::timeout(Duration::from_millis(50), outgoing.recv())
            .await
            .is_err(),
        "response-only resume emitted a notification"
    );
}

fn assert_requests(harness: &Harness, checkpoint_key: &str, expected_count: usize) {
    let requests = harness.requests.lock().expect("requests");
    assert_eq!(requests.len(), expected_count);
    for request in requests.iter() {
        assert_eq!(request.thread_id, harness.thread_id);
        assert_eq!(request.turn_id, harness.turn_id);
        assert_eq!(request.checkpoint_key, checkpoint_key);
    }
}

fn assert_sensitive_fields_absent(value: &Value) {
    let contract: Value = serde_json::from_str(CONTRACT_SOURCE).expect("contract");
    for field in contract["durableResume"]["sensitiveFieldsNeverProjected"]
        .as_array()
        .expect("sensitive fields")
    {
        assert!(
            !contains_key(value, field.as_str().expect("field")),
            "sensitive checkpoint field leaked: {field}"
        );
    }
}

fn contains_key(value: &Value, needle: &str) -> bool {
    match value {
        Value::Object(fields) => {
            fields.contains_key(needle) || fields.values().any(|value| contains_key(value, needle))
        }
        Value::Array(values) => values.iter().any(|value| contains_key(value, needle)),
        _ => false,
    }
}

