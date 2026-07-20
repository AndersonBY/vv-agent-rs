use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::mpsc;
use vv_agent::app_server::outgoing::{OutgoingEnvelope, OutgoingMessageSender};
use vv_agent::app_server::processor::MessageProcessor;
use vv_agent::app_server::protocol::{
    generate_app_server_json_schema_bundle, generate_app_server_typescript_bundle,
    map_run_event_to_notifications, AppItem, AppServerErrorCode, ApprovalDecision,
    ApprovalRequestParams, JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
    RequestId, SchemaExportResponse, ServerNotification, ServerRequest, ThreadStartParams,
    ThreadStartResponse, ThreadStatus, TurnCompletedParams, TurnStartParams, TurnStartResponse,
    TurnStatus,
};
use vv_agent::app_server::thread_store::SqliteThreadStore;
use vv_agent::app_server::transport::ConnectionId;
use vv_agent::{
    Agent, CacheUsage, CacheUsageStatus, LLMResponse, ModelRef, NoToolPolicy, RunBudgetLimits,
    RunConfig, RunEvent, Runner, ScriptedModelProvider, TokenUsage, ToolCall, UsageSource,
};

const CONTRACT_SOURCE: &str = include_str!("fixtures/parity/app_server_observable_v1.json");
const TOOL_METADATA_CONTRACT_SOURCE: &str = include_str!("fixtures/parity/tool_metadata_v1.json");

fn contract() -> Value {
    serde_json::from_str(CONTRACT_SOURCE).expect("valid App Server parity fixture")
}

#[test]
fn shared_fixture_enforces_json_rpc_version_and_request_ids() {
    let contract = contract();
    let json_rpc = &contract["jsonRpc"];
    let version = json_rpc["version"].as_str().expect("version");

    for request_id in json_rpc["validRequestIds"]
        .as_array()
        .expect("valid request ids")
    {
        let message: JsonRpcMessage = serde_json::from_value(json!({
            "jsonrpc": version,
            "id": request_id,
            "method": "model/list"
        }))
        .expect("valid request id");
        let JsonRpcMessage::Request(request) = message else {
            panic!("expected request");
        };
        assert_eq!(serde_json::to_value(request.id).expect("id"), *request_id);
    }

    for request_id in json_rpc["invalidRequestIds"]
        .as_array()
        .expect("invalid request ids")
    {
        let result = serde_json::from_value::<JsonRpcMessage>(json!({
            "jsonrpc": version,
            "id": request_id,
            "method": "model/list"
        }));
        assert!(result.is_err(), "request id must be rejected: {request_id}");
    }

    for payload in [
        json!({"id": 1, "method": "model/list"}),
        json!({"jsonrpc": "1.0", "id": 1, "method": "model/list"}),
    ] {
        assert!(serde_json::from_value::<JsonRpcMessage>(payload).is_err());
    }

    let error: JsonRpcMessage = serde_json::from_value(json!({
        "jsonrpc": version,
        "id": null,
        "error": {"code": -32700, "message": "Parse error"}
    }))
    .expect("error response may use null id");
    assert!(matches!(
        error,
        JsonRpcMessage::Error(error) if error.id == RequestId::Null
    ));
    assert!(serde_json::to_value(JsonRpcRequest {
        id: RequestId::Null,
        method: "model/list".to_string(),
        params: None,
    })
    .is_err());
    assert!(serde_json::to_value(JsonRpcResponse {
        id: RequestId::Null,
        result: json!({}),
    })
    .is_err());
}

#[test]
fn shared_fixture_requires_object_input_items() {
    let contract = contract();
    let valid = contract["input"]["valid"].clone();
    let params: TurnStartParams = serde_json::from_value(json!({
        "threadId": "thread_contract",
        "input": valid,
    }))
    .expect("object input items");
    assert!(params.input.iter().all(Value::is_object));

    for invalid_item in contract["input"]["invalid"]
        .as_array()
        .expect("invalid input items")
    {
        let result = serde_json::from_value::<TurnStartParams>(json!({
            "threadId": "thread_contract",
            "input": [invalid_item]
        }));
        assert!(
            result.is_err(),
            "input item must be rejected: {invalid_item}"
        );
    }
}

#[test]
fn shared_fixture_live_and_replay_payloads_use_epoch_seconds() {
    let contract = contract();
    let timestamps = &contract["timestamps"];
    assert_eq!(
        timestamps["eventMillis"].as_f64().expect("milliseconds") / 1000.0,
        timestamps["eventSeconds"].as_f64().expect("seconds")
    );

    let store = SqliteThreadStore::in_memory().expect("store");
    let thread = store
        .create_thread(ThreadStartParams::default())
        .expect("thread");
    let turn = store
        .create_turn(&thread.thread_id, Vec::new())
        .expect("turn");
    let mut expected = contract["liveReplay"]["item"]
        .as_object()
        .expect("item")
        .clone();
    expected.insert("threadId".to_string(), json!(thread.thread_id.clone()));
    expected.insert("turnId".to_string(), json!(turn.turn_id.clone()));
    let expected = Value::Object(expected);
    let item: AppItem = serde_json::from_value(expected.clone()).expect("fixture item");

    let live_payload = serde_json::to_value(&item).expect("live payload");
    store
        .append_item(&thread.thread_id, &turn.turn_id, item.clone())
        .expect("append item");
    let replayed = store.replay_items(&thread.thread_id).expect("replay");

    assert_eq!(live_payload, expected);
    assert_eq!(replayed, vec![item]);
    assert_eq!(
        contract["liveReplay"]["payloadMustMatch"],
        Value::Bool(true)
    );
}

#[test]
fn shared_fixture_tool_lifecycle_uses_additive_app_server_projection() {
    let contract = contract();
    let tool_metadata_contract: Value = serde_json::from_str(TOOL_METADATA_CONTRACT_SOURCE)
        .expect("valid tool metadata contract fixture");
    let projection_contract = &tool_metadata_contract["app_server_projection"];
    assert_eq!(projection_contract["tool_call_planned"], "no_notification");
    assert_eq!(
        projection_contract["planned_is_never_presented_as_execution_started"],
        Value::Bool(true)
    );
    assert!(projection_contract["tool_call_started"]
        .as_str()
        .expect("started projection rule")
        .contains("toolMetadata"));
    for field in [
        "directive",
        "errorCode",
        "executionStarted",
        "durationMs",
        "toolMetadata",
    ] {
        assert!(projection_contract["tool_call_completed"]
            .as_str()
            .expect("completed projection rule")
            .contains(field));
    }
    let lifecycle = &contract["toolLifecycle"];
    let metadata = lifecycle["plannedHasNoNotification"]["event"]["tool_metadata"].clone();

    let planned = run_event_from_fixture(lifecycle["plannedHasNoNotification"]["event"].clone());
    assert_eq!(
        mapped_notifications(&planned),
        expected_notifications(&lifecycle["plannedHasNoNotification"]["notifications"])
    );
    assert!(lifecycle["plannedHasNoNotification"]["persistedItem"].is_null());
    assert_eq!(
        lifecycle["plannedHasNoNotification"]["presentedAsExecution"],
        Value::Bool(false)
    );

    let started_expected = &lifecycle["executed"]["startedNotifications"];
    let started = run_event_from_fixture(json!({
        "type": "tool_call_started",
        "event_id": "evt_tool_started",
        "created_at": 100.1,
        "tool_name": "inspect",
        "tool_call_id": "call_tool",
        "arguments": {"path": "README.md"},
        "tool_metadata": metadata,
    }));
    let started_actual = mapped_notifications(&started);
    assert_eq!(started_actual, expected_notifications(started_expected));
    assert!(started_actual[0]["params"]["payload"]
        .get("toolMetadata")
        .is_some());

    let completed_expected = &lifecycle["executed"]["completedNotifications"];
    let completed = run_event_from_fixture(json!({
        "type": "tool_call_completed",
        "event_id": "evt_tool_completed",
        "created_at": 100.2,
        "tool_name": "inspect",
        "tool_call_id": "call_tool",
        "status": "success",
        "directive": "continue",
        "error_code": null,
        "execution_started": true,
        "duration_ms": 7,
        "tool_metadata": metadata,
    }));
    let completed_actual = mapped_notifications(&completed);
    assert_eq!(completed_actual, expected_notifications(completed_expected));
    for field in [
        "directive",
        "errorCode",
        "executionStarted",
        "durationMs",
        "toolMetadata",
    ] {
        assert!(
            completed_actual[0]["params"]["payload"]
                .get(field)
                .is_some(),
            "missing additive completed field {field}"
        );
    }

    assert_eq!(lifecycle["policyDenial"]["startedNotifications"], json!([]));
    let denied_expected = &lifecycle["policyDenial"]["completedNotifications"];
    let denied = run_event_from_fixture(json!({
        "type": "tool_call_completed",
        "event_id": "evt_tool_denied",
        "created_at": 100.3,
        "tool_name": "write_record",
        "tool_call_id": "call_denied",
        "status": "error",
        "directive": "continue",
        "error_code": "tool_not_allowed",
        "execution_started": false,
        "duration_ms": null,
        "tool_metadata": {
            "side_effect": "write",
            "idempotency": "unsupported",
            "terminal": false,
            "capability_tags": ["record.write"],
            "cost_dimensions": [],
        },
    }));
    assert_eq!(
        mapped_notifications(&denied),
        expected_notifications(denied_expected)
    );

    let legacy_expected = &lifecycle["legacyCompleted"]["notification"];
    let legacy = run_event_from_fixture(json!({
        "type": "tool_call_completed",
        "event_id": "evt_tool_legacy",
        "created_at": 99,
        "tool_name": "lookup",
        "tool_call_id": "call_legacy",
        "status": "success",
    }));
    assert_eq!(
        mapped_notifications(&legacy),
        expected_notifications(&Value::Array(vec![legacy_expected.clone()]))
    );
}

#[test]
fn shared_fixture_nullability_and_restart_recovery_match() {
    let contract = contract();
    let directory = tempfile::tempdir().expect("tempdir");
    let database = directory.path().join("app-server.sqlite3");
    let store = SqliteThreadStore::open(&database).expect("store");
    let thread = store
        .create_thread(ThreadStartParams::default())
        .expect("thread");
    let turn = store
        .create_turn(&thread.thread_id, Vec::new())
        .expect("turn");

    assert_fixture_fields(
        &serde_json::to_value(ThreadStartResponse::from_thread(&thread)).expect("thread start"),
        &contract["nullability"]["threadStartResponse"],
    );
    assert_fixture_fields(
        &serde_json::to_value(&thread).expect("thread snapshot"),
        &contract["nullability"]["threadSnapshot"],
    );
    assert_fixture_fields(
        &serde_json::to_value(&turn).expect("turn snapshot"),
        &contract["nullability"]["turnSnapshot"],
    );

    let terminal = serde_json::to_value(TurnCompletedParams {
        thread_id: thread.thread_id.clone(),
        turn_id: turn.turn_id,
        run_id: None,
        status: TurnStatus::Failed,
        final_output: None,
        completion_reason: None,
        completion_tool_name: None,
        partial_output: None,
        error: None,
        token_usage: None,
        budget_usage: None,
        budget_exhaustion: None,
        checkpoint: None,
        interruption: None,
    })
    .expect("terminal payload");
    for field in contract["terminal"]["optionalFieldsOmittedWhenAbsent"]
        .as_array()
        .expect("optional terminal fields")
    {
        let field = field.as_str().expect("field name");
        assert!(
            terminal.get(field).is_none(),
            "unexpected terminal field: {field}"
        );
    }

    store
        .set_active_turn(&thread.thread_id, Some("turn_stale"), ThreadStatus::Running)
        .expect("mark stale running thread");
    drop(store);
    let restarted = SqliteThreadStore::open(&database).expect("restarted store");
    let recovered = restarted
        .get_thread(&thread.thread_id)
        .expect("read recovered thread")
        .expect("recovered thread");
    assert_eq!(
        serde_json::to_value(recovered.status).expect("status"),
        contract["restart"]["staleRunningThreadStatus"]
    );
}

#[tokio::test]
async fn budget_exhaustion_projects_typed_usage_to_turn_and_store() {
    let expected = contract()["terminal"]["agentStatusProjection"]
        .as_array()
        .expect("status projections")
        .iter()
        .find(|case| case["name"] == "budget_exhaustion_is_failed_with_typed_observation")
        .expect("budget status projection")
        .clone();
    let mut response = LLMResponse::new("draft over budget");
    response.token_usage = TokenUsage {
        prompt_tokens: 12,
        total_tokens: 12,
        usage_source: UsageSource::ProviderReported,
        cache_usage: CacheUsage {
            status: CacheUsageStatus::ProviderReported,
            uncached_input_tokens: Some(12),
            ..CacheUsage::default()
        },
        ..TokenUsage::default()
    };
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "budget-model",
            vec![response],
        ))
        .workspace(".")
        .default_run_config(
            RunConfig::builder()
                .budget_limits(
                    RunBudgetLimits::builder()
                        .max_total_tokens(10)
                        .build()
                        .expect("budget limits"),
                )
                .build(),
        )
        .build()
        .expect("runner");
    let agent = Agent::builder("assistant")
        .instructions("Return the scripted draft.")
        .model(ModelRef::named("budget-model"))
        .no_tool_policy(NoToolPolicy::Finish)
        .build()
        .expect("agent");
    let store = SqliteThreadStore::in_memory().expect("store");
    let (mut processor, mut outgoing) =
        MessageProcessor::new_for_tests_with_runtime(64, runner, agent, store.clone());
    let connection_id = ConnectionId::new(1);
    initialize(&mut processor, &mut outgoing, connection_id, 1).await;

    processor
        .process_message(
            connection_id,
            request(2, "thread/start", json!({"agentKey": "default"})),
        )
        .await;
    let thread: ThreadStartResponse = decode_response(expect_response(&mut outgoing).await);
    let _ = expect_notification(&mut outgoing).await;
    processor
        .process_message(
            connection_id,
            request(
                3,
                "turn/start",
                json!({
                    "threadId": thread.thread_id,
                    "input": [{"type": "text", "text": "run"}]
                }),
            ),
        )
        .await;
    let turn: TurnStartResponse = decode_response(expect_response(&mut outgoing).await);

    let completed = loop {
        if let ServerNotification::TurnCompleted(completed) =
            expect_notification(&mut outgoing).await
        {
            break completed;
        }
    };

    assert_eq!(completed.status, TurnStatus::Failed);
    assert_eq!(expected["turnStatus"], "failed");
    assert_eq!(
        completed.completion_reason.as_deref(),
        Some("budget_exhausted")
    );
    assert!(completed.error.is_some());
    let budget_usage = completed.budget_usage.expect("budget usage");
    assert_eq!(budget_usage["cycles"], 1);
    assert_eq!(budget_usage["total_tokens"], 12);
    let budget_exhaustion = completed.budget_exhaustion.expect("budget exhaustion");
    assert_eq!(budget_exhaustion["dimension"], "total_tokens");
    assert_eq!(budget_exhaustion["limit"], 10);
    assert_eq!(budget_exhaustion["observed"], 12);

    let stored_turn = store
        .list_turns(&thread.thread_id)
        .expect("stored turns")
        .into_iter()
        .find(|stored| stored.turn_id == turn.turn_id)
        .expect("stored turn");
    assert_eq!(stored_turn.status, TurnStatus::Failed);
    assert_eq!(stored_turn.result["budgetUsage"], json!(budget_usage));
    assert_eq!(
        stored_turn.result["budgetExhaustion"],
        json!(budget_exhaustion)
    );
}

#[tokio::test]
async fn shared_fixture_rejects_duplicate_server_request_ids_and_cleans_disconnects() {
    let contract = contract();
    let (outgoing, mut receiver) = OutgoingMessageSender::channel(8);
    let connection_id = ConnectionId::new(1);
    outgoing.register_connection(connection_id).await;
    let request_id = RequestId::String("srvreq_1".to_string());
    let callback = outgoing
        .send_server_request_with_id(connection_id, request_id, approval_request())
        .await
        .expect("first request");
    let _ = receiver.recv().await.expect("outgoing request");
    assert_eq!(outgoing.pending_server_request_count().await, 1);

    let duplicate = outgoing
        .send_server_request(connection_id, approval_request())
        .await
        .expect_err("generated duplicate id");
    assert_eq!(duplicate.code(), AppServerErrorCode::InvalidParams);
    assert_eq!(contract["approval"]["duplicateServerRequestId"], "reject");
    assert_eq!(outgoing.pending_server_request_count().await, 1);

    outgoing.unregister_connection(connection_id).await;
    assert_eq!(outgoing.pending_server_request_count().await, 0);
    let disconnected = callback
        .await
        .expect("callback delivered")
        .expect_err("disconnect error");
    assert_eq!(disconnected.message, "client_disconnected");
    assert_eq!(contract["approval"]["disconnectDecision"], "timeout");

    for decision in contract["approval"]["decisions"]
        .as_array()
        .expect("approval decisions")
    {
        let wire = decision.as_str().expect("decision");
        let parsed: ApprovalDecision =
            serde_json::from_value(decision.clone()).expect("canonical decision");
        assert_eq!(parsed.as_wire(), wire);
        assert!(
            serde_json::from_value::<ApprovalDecision>(json!(wire.to_ascii_uppercase())).is_err()
        );
    }
    assert_eq!(contract["approval"]["caseSensitive"], Value::Bool(true));
}

#[tokio::test]
async fn shared_fixture_orders_thread_turn_and_terminal_messages() {
    let contract = contract();
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "contract-model",
            vec![finish_response("contract payload")],
        ))
        .workspace(".")
        .build()
        .expect("runner");
    let agent = Agent::builder("assistant")
        .instructions("Finish the contract turn.")
        .model(ModelRef::named("contract-model"))
        .build()
        .expect("agent");
    let (mut processor, mut outgoing) = MessageProcessor::new_for_tests_with_runtime(
        64,
        runner,
        agent,
        SqliteThreadStore::in_memory().expect("store"),
    );
    let connection_id = ConnectionId::new(1);
    initialize(&mut processor, &mut outgoing, connection_id, 1).await;

    processor
        .process_message(
            connection_id,
            request(2, "thread/start", json!({"agentKey": "default"})),
        )
        .await;
    let thread_response = expect_response(&mut outgoing).await;
    let thread: ThreadStartResponse = decode_response(thread_response);
    let thread_started = expect_notification(&mut outgoing).await;
    assert!(matches!(
        thread_started,
        ServerNotification::ThreadStarted(_)
    ));
    assert_eq!(
        contract["ordering"]["threadStart"],
        json!(["response", "thread/started"])
    );

    processor
        .process_message(
            connection_id,
            request(
                3,
                "turn/start",
                json!({
                    "threadId": thread.thread_id,
                    "input": contract["input"]["valid"].clone()
                }),
            ),
        )
        .await;
    let turn: TurnStartResponse = decode_response(expect_response(&mut outgoing).await);
    let running = expect_notification(&mut outgoing).await;
    let started = expect_notification(&mut outgoing).await;
    assert!(matches!(
        running,
        ServerNotification::ThreadStatusChanged(ref status)
            if status.status == ThreadStatus::Running
    ));
    assert!(matches!(started, ServerNotification::TurnStarted(_)));
    assert_eq!(
        contract["ordering"]["turnStart"],
        json!(["response", "thread/status/changed", "turn/started"])
    );

    let mut terminal_order = Vec::new();
    loop {
        match expect_notification(&mut outgoing).await {
            ServerNotification::ThreadStatusChanged(status)
                if status.status == ThreadStatus::Idle =>
            {
                terminal_order.push("thread/status/changed");
            }
            ServerNotification::TurnCompleted(completed) => {
                assert_eq!(completed.turn_id, turn.turn_id);
                terminal_order.push("turn/completed");
                break;
            }
            _ => {}
        }
    }
    assert_eq!(
        terminal_order,
        contract["ordering"]["turnTerminal"]
            .as_array()
            .expect("terminal order")
            .iter()
            .map(|value| value.as_str().expect("method"))
            .collect::<Vec<_>>()
    );
    assert_eq!(contract["terminal"]["threadStatusAfterTurn"], "idle");
}

#[tokio::test]
async fn shared_fixture_allows_connection_reinitialize_and_exports_exact_schema_bundle() {
    let contract = contract();
    let (mut processor, mut outgoing) = MessageProcessor::new_for_tests(16);
    let connection_id = ConnectionId::new(1);
    initialize(&mut processor, &mut outgoing, connection_id, 1).await;
    processor.disconnect_connection(connection_id).await;
    initialize(&mut processor, &mut outgoing, connection_id, 2).await;
    assert_eq!(
        contract["restart"]["connectionIdCanReinitialize"],
        Value::Bool(true)
    );

    processor
        .process_message(connection_id, request(3, "schema/export", json!({})))
        .await;
    let exported: SchemaExportResponse = decode_response(expect_response(&mut outgoing).await);
    let json_schema = generate_app_server_json_schema_bundle().expect("JSON schema bundle");
    let typescript = generate_app_server_typescript_bundle().expect("TypeScript bundle");
    assert_eq!(exported.json_schema, json_schema);
    assert_eq!(exported.typescript, typescript);

    let expected_json = contract["schema"]["json"]
        .as_array()
        .expect("JSON schema names")
        .iter()
        .map(|name| {
            name.as_str()
                .expect("JSON schema name")
                .trim_end_matches(".json")
                .to_string()
        })
        .collect::<Vec<_>>();
    let expected_typescript = contract["schema"]["typescript"]
        .as_array()
        .expect("TypeScript schema names")
        .iter()
        .map(|name| name.as_str().expect("TypeScript schema name").to_string())
        .collect::<Vec<_>>();
    assert_eq!(json_schema.len(), 19);
    assert_eq!(typescript.len(), 18);
    assert_eq!(
        json_schema.keys().cloned().collect::<Vec<_>>(),
        expected_json
    );
    assert_eq!(
        typescript.keys().cloned().collect::<Vec<_>>(),
        expected_typescript
    );
    let first_typescript = typescript.values().next().expect("TypeScript source");
    for source in typescript.values() {
        assert_eq!(source, first_typescript);
        assert!(!source.contains("import "));
        assert!(!source.contains("bigint"));
    }
    let notification_schema: Value = serde_json::from_str(
        json_schema
            .get("ServerNotification")
            .expect("ServerNotification schema"),
    )
    .expect("ServerNotification JSON schema");
    let completion_properties = &notification_schema["$defs"]["TurnCompletedParams"]["properties"];
    for field in ["completionReason", "completionToolName", "partialOutput"] {
        assert!(
            completion_properties.get(field).is_some(),
            "missing TurnCompletedParams field: {field}"
        );
        assert!(
            first_typescript.contains(&format!("{field}?: string")),
            "missing TypeScript completion field: {field}"
        );
    }
    for field in ["budgetUsage", "budgetExhaustion"] {
        assert_eq!(
            completion_properties[field]["type"], "object",
            "budget observation must be an object: {field}"
        );
        assert!(
            first_typescript.contains(&format!("{field}?: JsonObject")),
            "missing TypeScript budget field: {field}"
        );
    }

    processor
        .process_message(
            connection_id,
            request(4, "schema/export", json!({"unexpected": true})),
        )
        .await;
    let error = expect_error(&mut outgoing).await;
    assert_eq!(error.error.code, AppServerErrorCode::InvalidParams.code());
}

fn assert_fixture_fields(actual: &Value, expected: &Value) {
    for (field, expected_value) in expected.as_object().expect("fixture fields") {
        assert_eq!(actual.get(field), Some(expected_value), "field: {field}");
    }
}

fn run_event_from_fixture(mut event: Value) -> RunEvent {
    let object = event.as_object_mut().expect("tool lifecycle event object");
    object
        .entry("version".to_string())
        .or_insert_with(|| json!("v1"));
    object
        .entry("run_id".to_string())
        .or_insert_with(|| json!("run_tool"));
    object
        .entry("trace_id".to_string())
        .or_insert_with(|| json!("trace_tool"));
    serde_json::from_value(event).expect("valid fixture-backed run event")
}

fn mapped_notifications(event: &RunEvent) -> Value {
    Value::Array(
        map_run_event_to_notifications("thread-tool", "turn-tool", event)
            .into_iter()
            .map(|notification| {
                let mut value =
                    serde_json::to_value(notification).expect("server notification serializes");
                value
                    .as_object_mut()
                    .expect("server notification object")
                    .insert("jsonrpc".to_string(), json!("2.0"));
                value
            })
            .collect(),
    )
}

fn expected_notifications(notifications: &Value) -> Value {
    let mut expected = notifications.clone();
    for notification in expected.as_array_mut().expect("notification array") {
        for field in ["createdAt", "updatedAt"] {
            let timestamp = &mut notification["params"][field];
            if let Some(value) = timestamp.as_f64() {
                *timestamp = json!(value);
            }
        }
    }
    expected
}

fn approval_request() -> ServerRequest {
    ServerRequest::ApprovalRequest(ApprovalRequestParams {
        thread_id: "thread_1".to_string(),
        turn_id: "turn_1".to_string(),
        request_id: "approval_1".to_string(),
        tool_call_id: "call_1".to_string(),
        tool_name: "dangerous".to_string(),
        preview: "dangerous {}".to_string(),
        arguments: json!({}),
    })
}

fn request(id: i64, method: &str, params: Value) -> JsonRpcMessage {
    JsonRpcMessage::Request(JsonRpcRequest {
        id: RequestId::Integer(id),
        method: method.to_string(),
        params: Some(params),
    })
}

async fn initialize(
    processor: &mut MessageProcessor,
    outgoing: &mut mpsc::Receiver<OutgoingEnvelope>,
    connection_id: ConnectionId,
    request_id: i64,
) {
    processor
        .process_message(
            connection_id,
            request(
                request_id,
                "initialize",
                json!({"clientInfo": {"name": "contract-test"}}),
            ),
        )
        .await;
    let _ = expect_response(outgoing).await;
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

async fn expect_response(rx: &mut mpsc::Receiver<OutgoingEnvelope>) -> JsonRpcResponse {
    let envelope = next_envelope(rx).await;
    let JsonRpcMessage::Response(response) = envelope.message else {
        panic!("expected response, got {:?}", envelope.message);
    };
    response
}

async fn expect_error(
    rx: &mut mpsc::Receiver<OutgoingEnvelope>,
) -> vv_agent::app_server::protocol::JsonRpcError {
    let envelope = next_envelope(rx).await;
    let JsonRpcMessage::Error(error) = envelope.message else {
        panic!("expected error, got {:?}", envelope.message);
    };
    error
}

async fn expect_notification(rx: &mut mpsc::Receiver<OutgoingEnvelope>) -> ServerNotification {
    let envelope = next_envelope(rx).await;
    let JsonRpcMessage::Notification(notification) = envelope.message else {
        panic!("expected notification, got {:?}", envelope.message);
    };
    let value = match notification.params {
        Some(params) => json!({"method": notification.method, "params": params}),
        None => json!({"method": notification.method}),
    };
    serde_json::from_value(value).expect("typed server notification")
}

async fn next_envelope(rx: &mut mpsc::Receiver<OutgoingEnvelope>) -> OutgoingEnvelope {
    tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .expect("message timeout")
        .expect("outgoing message")
}

fn decode_response<T: serde::de::DeserializeOwned>(response: JsonRpcResponse) -> T {
    serde_json::from_value(response.result).expect("response payload")
}

fn finish_response(message: &str) -> LLMResponse {
    let mut arguments = BTreeMap::new();
    arguments.insert("message".to_string(), json!(message));
    LLMResponse::with_tool_calls(
        message,
        vec![ToolCall::new("finish", "task_finish", arguments)],
    )
}
