use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::mpsc;
use vv_agent::app_server::outgoing::OutgoingEnvelope;
use vv_agent::app_server::processor::MessageProcessor;
use vv_agent::app_server::protocol::{
    map_run_event_to_notifications, JsonRpcMessage, JsonRpcRequest, JsonRpcResponse, RequestId,
    ServerNotification,
};
use vv_agent::app_server::thread_store::SqliteThreadStore;
use vv_agent::app_server::transport::ConnectionId;
use vv_agent::{
    Agent, CacheUsage, CacheUsageStatus, FunctionTool, LLMResponse, LlmRequest, ModelRef, RunEvent,
    Runner, ScriptStep, ScriptedModelProvider, TokenUsage, ToolCall, ToolOutput, UsageSource,
};

#[tokio::test]
async fn initialize_matches_python_v1_and_does_not_advertise_missing_runtime() {
    let (mut processor, mut outgoing) = MessageProcessor::new_for_tests(16);
    let connection_id = ConnectionId::new(1);

    processor
        .process_message(connection_id, initialize_request(1))
        .await;

    let result = expect_response_value(&mut outgoing).await;
    assert_eq!(result["userAgent"], "vv-agent-app-server");
    assert_eq!(result["protocolVersion"], "v1");
    assert_eq!(result["capabilities"]["modelList"], true);
    assert_eq!(result["capabilities"]["threadLifecycle"], false);
    assert_eq!(result["capabilities"]["notificationOptOut"], true);
    assert_eq!(result["capabilities"]["schemaExport"], true);
    assert_eq!(result["capabilities"]["approvalResolve"], true);
    assert!(result.get("serverInfo").is_none());
    assert!(result.get("supportedTransports").is_none());
}

#[tokio::test]
async fn thread_start_and_unsubscribe_use_python_v1_payloads() {
    let (mut processor, mut outgoing) = processor_with_runtime(Vec::new());
    let connection_id = ConnectionId::new(1);
    initialize(&mut processor, &mut outgoing, connection_id).await;

    processor
        .process_message(
            connection_id,
            request(
                2,
                "thread/start",
                json!({
                    "agentKey": "default",
                    "cwd": "/tmp/project",
                    "metadata": {"source": "python-fixture"}
                }),
            ),
        )
        .await;

    let started = expect_response_value(&mut outgoing).await;
    assert_eq!(started["threadId"], "thread_1");
    assert_eq!(started["agentKey"], "default");
    assert_eq!(started["cwd"], "/tmp/project");
    assert_eq!(started["status"], "idle");
    assert!(started.get("thread").is_none());
    let notification = expect_notification_value(&mut outgoing).await;
    assert_eq!(notification["method"], "thread/started");
    assert_eq!(notification["params"], started);

    processor
        .process_message(
            connection_id,
            request(3, "thread/unsubscribe", json!({"threadId": "thread_1"})),
        )
        .await;

    let unsubscribed = expect_response_value(&mut outgoing).await;
    assert_eq!(
        unsubscribed,
        json!({"threadId": "thread_1", "subscribed": false, "closed": true})
    );
    let closed = expect_notification_value(&mut outgoing).await;
    assert_eq!(closed["method"], "thread/closed");
    assert_eq!(closed["params"], json!({"threadId": "thread_1"}));
    let status = expect_notification_value(&mut outgoing).await;
    assert_eq!(status["method"], "thread/status/changed");
    assert_eq!(status["params"]["status"], "closed");
}

#[tokio::test]
async fn turn_controls_match_python_v1_errors_without_an_active_turn() {
    let (mut processor, mut outgoing) = processor_with_runtime(Vec::new());
    let connection_id = ConnectionId::new(1);
    initialize(&mut processor, &mut outgoing, connection_id).await;
    start_thread(&mut processor, &mut outgoing, connection_id).await;

    for (id, method) in [(3, "turn/steer"), (4, "turn/followUp")] {
        processor
            .process_message(
                connection_id,
                request(
                    id,
                    method,
                    json!({
                        "threadId": "thread_1",
                        "expectedTurnId": "turn_1",
                        "input": [{"type": "text", "text": "continue"}]
                    }),
                ),
            )
            .await;
        let error = expect_error_value(&mut outgoing).await;
        assert_eq!(error["code"], -32030, "method: {method}");
        assert_eq!(
            error["message"], "Active turn not found",
            "method: {method}"
        );
    }

    processor
        .process_message(
            connection_id,
            request(
                5,
                "turn/interrupt",
                json!({
                    "threadId": "thread_1",
                    "expectedTurnId": "turn_1",
                    "reason": "stop"
                }),
            ),
        )
        .await;
    let error = expect_error_value(&mut outgoing).await;
    assert_eq!(error["code"], -32030);
    assert_eq!(error["message"], "Active turn not found");
}

#[tokio::test]
async fn turn_completion_keeps_python_v1_terminal_fields() {
    let mut response = finish_response("done");
    response.token_usage = TokenUsage {
        prompt_tokens: 10,
        completion_tokens: 2,
        total_tokens: 12,
        input_tokens: 10,
        output_tokens: 2,
        usage_source: UsageSource::ProviderReported,
        cache_usage: CacheUsage {
            status: CacheUsageStatus::ProviderReported,
            read_tokens: Some(0),
            write_tokens: None,
            uncached_input_tokens: Some(10),
            source: Some("provider_usage".to_string()),
        },
        ..TokenUsage::default()
    };
    let (mut processor, mut outgoing) = processor_with_runtime(vec![response]);
    let connection_id = ConnectionId::new(1);
    initialize(&mut processor, &mut outgoing, connection_id).await;
    start_thread(&mut processor, &mut outgoing, connection_id).await;

    processor
        .process_message(
            connection_id,
            request(
                3,
                "turn/start",
                json!({
                    "threadId": "thread_1",
                    "input": [{"type": "text", "text": "hello"}]
                }),
            ),
        )
        .await;

    let started = expect_response_value(&mut outgoing).await;
    assert_eq!(started["threadId"], "thread_1");
    assert_eq!(started["turnId"], "turn_1");
    assert_eq!(started["status"], "running");
    assert!(started.get("turn").is_none());

    let completed = next_notification(&mut outgoing, "turn/completed").await;
    let params = &completed["params"];
    assert_eq!(params["threadId"], "thread_1");
    assert_eq!(params["turnId"], "turn_1");
    assert_eq!(params["status"], "completed");
    assert_eq!(params["finalOutput"], "done");
    assert!(params["runId"]
        .as_str()
        .is_some_and(|run_id| run_id.starts_with("run_")));
    assert_ne!(params["runId"], "assistant_run");
    assert!(params.get("tokenUsage").is_some());
    assert_eq!(
        params["tokenUsage"]["cache_usage"]["status"],
        "provider_reported"
    );
    assert_eq!(params["tokenUsage"]["cache_usage"]["read_tokens"], 0);
    assert!(params.get("turn").is_none());
}

#[tokio::test]
async fn steer_updates_active_turn_and_follow_up_starts_next_turn() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let requests = Arc::new(Mutex::new(Vec::<LlmRequest>::new()));
    let steps = (0..3)
        .map(|index| {
            let requests = requests.clone();
            ScriptStep::callback(move |request| {
                requests.lock().expect("requests").push(request.clone());
                Ok(match index {
                    0 => LLMResponse::with_tool_calls(
                        "working",
                        vec![ToolCall::new("call_1", "slow_tool", BTreeMap::new())],
                    ),
                    1 => finish_response("first done"),
                    _ => finish_response("follow-up done"),
                })
            })
        })
        .collect();
    let provider = ScriptedModelProvider::from_steps("scripted", "demo-model", steps);
    let gate_for_tool = gate.clone();
    let slow_tool = FunctionTool::builder("slow_tool")
        .description("Wait for the compatibility test gate.")
        .json_schema(json!({"type": "object", "properties": {}, "required": []}))
        .needs_approval(true)
        .handler(move |_context, _arguments: Value| {
            let gate = gate_for_tool.clone();
            async move {
                gate.notified().await;
                Ok(ToolOutput::text("released"))
            }
        })
        .build()
        .expect("slow tool");
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(".")
        .build()
        .expect("runner");
    let agent = Agent::builder("assistant")
        .instructions("Use the tool, then finish.")
        .model(ModelRef::named("demo-model"))
        .tool(slow_tool)
        .build()
        .expect("agent");
    let (mut processor, mut outgoing) = MessageProcessor::new_for_tests_with_runtime(
        128,
        runner,
        agent,
        SqliteThreadStore::in_memory().expect("store"),
    );
    let connection_id = ConnectionId::new(1);
    initialize(&mut processor, &mut outgoing, connection_id).await;
    start_thread(&mut processor, &mut outgoing, connection_id).await;
    processor
        .process_message(
            connection_id,
            request(
                3,
                "turn/start",
                json!({
                    "threadId": "thread_1",
                    "input": [{"type": "text", "text": "initial"}]
                }),
            ),
        )
        .await;
    let _ = expect_response_value(&mut outgoing).await;
    let _ = next_notification(&mut outgoing, "item/started").await;
    let approval_request_id = next_server_request_id(&mut outgoing, "approval/request").await;
    processor
        .process_message(
            connection_id,
            JsonRpcMessage::Response(JsonRpcResponse {
                id: approval_request_id,
                result: json!({"decision": "allow"}),
            }),
        )
        .await;

    processor
        .process_message(
            connection_id,
            request(
                4,
                "turn/steer",
                json!({
                    "threadId": "thread_1",
                    "expectedTurnId": "turn_1",
                    "input": [{"type": "text", "text": "steered"}]
                }),
            ),
        )
        .await;
    let steer = expect_response_value(&mut outgoing).await;
    assert_eq!(
        steer,
        json!({"threadId": "thread_1", "turnId": "turn_1", "queued": true})
    );

    processor
        .process_message(
            connection_id,
            request(
                5,
                "turn/followUp",
                json!({
                    "threadId": "thread_1",
                    "expectedTurnId": "turn_1",
                    "input": [{"type": "text", "text": "continue"}]
                }),
            ),
        )
        .await;
    let follow_up = expect_response_value(&mut outgoing).await;
    assert_eq!(
        follow_up,
        json!({"threadId": "thread_1", "turnId": "turn_1", "queued": true})
    );

    gate.notify_one();
    let mut completed_turns = Vec::new();
    while completed_turns.len() < 2 {
        let envelope = tokio::time::timeout(Duration::from_secs(5), outgoing.recv())
            .await
            .expect("turn lifecycle timeout")
            .expect("outgoing message");
        match envelope.message {
            JsonRpcMessage::Request(request) if request.method == "approval/request" => {
                processor
                    .process_message(
                        connection_id,
                        JsonRpcMessage::Response(JsonRpcResponse {
                            id: request.id,
                            result: json!({"decision": "allow"}),
                        }),
                    )
                    .await;
            }
            JsonRpcMessage::Notification(notification)
                if notification.method == "turn/completed" =>
            {
                let params = notification.params.expect("turn/completed params");
                completed_turns.push(params["turnId"].clone());
            }
            _ => {}
        }
    }
    assert_eq!(completed_turns, vec![json!("turn_1"), json!("turn_2")]);

    let requests = requests.lock().expect("requests");
    assert!(requests[1]
        .messages
        .iter()
        .any(|message| message.content == "steered"));
    assert!(requests[2]
        .messages
        .iter()
        .any(|message| message.content == "continue"));
}

#[test]
fn tool_started_event_emits_real_tool_delta_with_python_v1_item_fields() {
    let event = RunEvent::tool_call_started(
        "run_1",
        "trace_1",
        "assistant",
        1,
        "call_1",
        "bash",
        json!({"cmd": "cargo test"}),
    );

    let notifications = map_run_event_to_notifications("thread_1", "turn_1", &event);
    let values = notifications
        .iter()
        .map(|notification| serde_json::to_value(notification).expect("notification"))
        .collect::<Vec<_>>();

    let started = values
        .iter()
        .find(|value| value["method"] == "item/started")
        .expect("item/started");
    assert_eq!(
        started["params"]["itemId"],
        format!("item_{}", event.event_id().as_str())
    );
    assert_eq!(started["params"]["threadId"], "thread_1");
    assert_eq!(started["params"]["turnId"], "turn_1");
    assert_eq!(started["params"]["type"], "toolCall");
    assert_eq!(started["params"]["payload"]["toolCallId"], "call_1");

    let delta = values
        .iter()
        .find(|value| value["method"] == "item/toolCall/delta")
        .expect("item/toolCall/delta");
    assert_eq!(
        delta["params"]["itemId"],
        format!("item_{}", event.event_id().as_str())
    );
    assert_eq!(delta["params"]["delta"], json!({"cmd": "cargo test"}));
}

#[test]
fn approval_request_matches_python_v1_payload() {
    let event = RunEvent::approval_requested(
        "run_1",
        "trace_1",
        "assistant",
        "approval_1",
        "call_1",
        "bash",
        "Run cargo test",
    );

    let notifications = map_run_event_to_notifications("thread_1", "turn_1", &event);
    let params = notifications
        .iter()
        .find_map(|notification| match notification {
            ServerNotification::ApprovalRequested(params) => Some(params),
            _ => None,
        })
        .expect("approval notification");
    let value = serde_json::to_value(params).expect("approval params");

    assert_eq!(value["requestId"], "approval_1");
    assert_eq!(value["threadId"], "thread_1");
    assert_eq!(value["turnId"], "turn_1");
    assert_eq!(value["toolCallId"], "call_1");
    assert_eq!(value["toolName"], "bash");
    assert_eq!(value["arguments"], json!({}));
    assert!(value.get("choices").is_none());
}

fn processor_with_runtime(
    responses: Vec<LLMResponse>,
) -> (MessageProcessor, mpsc::Receiver<OutgoingEnvelope>) {
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "demo-model",
            responses,
        ))
        .workspace(".")
        .build()
        .expect("runner");
    let agent = Agent::builder("assistant")
        .instructions("Answer, then finish.")
        .model(ModelRef::named("demo-model"))
        .build()
        .expect("agent");
    MessageProcessor::new_for_tests_with_runtime(
        128,
        runner,
        agent,
        SqliteThreadStore::in_memory().expect("store"),
    )
}

async fn initialize(
    processor: &mut MessageProcessor,
    outgoing: &mut mpsc::Receiver<OutgoingEnvelope>,
    connection_id: ConnectionId,
) {
    processor
        .process_message(connection_id, initialize_request(1))
        .await;
    let _ = expect_response_value(outgoing).await;
}

async fn start_thread(
    processor: &mut MessageProcessor,
    outgoing: &mut mpsc::Receiver<OutgoingEnvelope>,
    connection_id: ConnectionId,
) {
    processor
        .process_message(
            connection_id,
            request(2, "thread/start", json!({"agentKey": "default"})),
        )
        .await;
    let _ = expect_response_value(outgoing).await;
    let _ = expect_notification_value(outgoing).await;
}

fn initialize_request(id: i64) -> JsonRpcMessage {
    request(
        id,
        "initialize",
        json!({
            "clientInfo": {"name": "python-v1-fixture"},
            "capabilities": {"optOutNotificationMethods": []}
        }),
    )
}

fn request(id: i64, method: &str, params: Value) -> JsonRpcMessage {
    JsonRpcMessage::Request(JsonRpcRequest {
        id: RequestId::Integer(id),
        method: method.to_string(),
        params: Some(params),
    })
}

async fn expect_response_value(rx: &mut mpsc::Receiver<OutgoingEnvelope>) -> Value {
    loop {
        let envelope = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("message timeout")
            .expect("outgoing message");
        if let JsonRpcMessage::Response(response) = envelope.message {
            return response.result;
        }
    }
}

async fn expect_error_value(rx: &mut mpsc::Receiver<OutgoingEnvelope>) -> Value {
    loop {
        let envelope = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("message timeout")
            .expect("outgoing message");
        if let JsonRpcMessage::Error(error) = envelope.message {
            return serde_json::to_value(error.error).expect("error body");
        }
    }
}

async fn expect_notification_value(rx: &mut mpsc::Receiver<OutgoingEnvelope>) -> Value {
    loop {
        let envelope = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("message timeout")
            .expect("outgoing message");
        if let JsonRpcMessage::Notification(notification) = envelope.message {
            return serde_json::to_value(notification).expect("notification");
        }
    }
}

async fn next_notification(rx: &mut mpsc::Receiver<OutgoingEnvelope>, method: &str) -> Value {
    loop {
        let value = expect_notification_value(rx).await;
        if value["method"] == method {
            return value;
        }
    }
}

async fn next_server_request_id(
    rx: &mut mpsc::Receiver<OutgoingEnvelope>,
    method: &str,
) -> RequestId {
    loop {
        let envelope = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("server request timeout")
            .expect("outgoing message");
        if let JsonRpcMessage::Request(request) = envelope.message {
            if request.method == method {
                return request.id;
            }
        }
    }
}

fn finish_response(message: &str) -> LLMResponse {
    let args = BTreeMap::from([("message".to_string(), json!(message))]);
    LLMResponse::with_tool_calls(message, vec![ToolCall::new("finish", "task_finish", args)])
}
