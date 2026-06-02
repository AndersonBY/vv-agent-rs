use serde_json::json;
use vv_agent::app_server::protocol::{
    AppClientCapabilities, AppClientInfo, AppServerCapabilities, AppServerErrorCode,
    ApprovalDecision, ApprovalRequestParams, ClientRequest, InitializeParams, InitializeResponse,
    JsonRpcError, JsonRpcMessage, JsonRpcRequest, RequestId, ServerNotification, ServerRequest,
    ThreadStartParams,
};

#[test]
fn json_rpc_request_round_trips_without_jsonrpc_header() {
    let request = JsonRpcRequest {
        id: RequestId::Integer(1),
        method: "initialize".to_string(),
        params: Some(json!({"clientInfo": {"name": "test"}})),
    };

    let value = serde_json::to_value(&request).expect("serialize");
    assert_eq!(value["id"], 1);
    assert_eq!(value["method"], "initialize");
    assert!(value.get("jsonrpc").is_none());

    let decoded: JsonRpcRequest = serde_json::from_value(value).expect("deserialize");
    assert_eq!(decoded.id, RequestId::Integer(1));
}

#[test]
fn json_rpc_message_decodes_request_notification_response_and_error() {
    let request: JsonRpcMessage =
        serde_json::from_value(json!({"id": 1, "method": "initialize"})).expect("request");
    assert!(matches!(request, JsonRpcMessage::Request(_)));

    let notification: JsonRpcMessage =
        serde_json::from_value(json!({"method": "initialized"})).expect("notification");
    assert!(matches!(notification, JsonRpcMessage::Notification(_)));

    let response: JsonRpcMessage =
        serde_json::from_value(json!({"id": 1, "result": {}})).expect("response");
    assert!(matches!(response, JsonRpcMessage::Response(_)));

    let error: JsonRpcMessage = serde_json::from_value(json!({
        "id": 1,
        "error": {"code": -32010, "message": "Not initialized"}
    }))
    .expect("error");
    assert!(matches!(error, JsonRpcMessage::Error(_)));
}

#[test]
fn app_server_error_code_values_are_stable() {
    assert_eq!(AppServerErrorCode::ServerOverloaded.code(), -32001);
    assert_eq!(AppServerErrorCode::NotInitialized.code(), -32010);
    assert_eq!(AppServerErrorCode::AlreadyInitialized.code(), -32011);
}

#[test]
fn error_response_preserves_code_message_and_data() {
    let error = JsonRpcError::new(
        RequestId::Integer(2),
        AppServerErrorCode::NotInitialized,
        "Not initialized",
    )
    .with_data(json!({"method": "thread/start"}));

    let value = serde_json::to_value(&error).expect("serialize");
    assert_eq!(value["id"], 2);
    assert_eq!(value["error"]["code"], -32010);
    assert_eq!(value["error"]["data"]["method"], "thread/start");
}

#[test]
fn stable_client_request_methods_cover_mvp_surface() {
    let methods = ClientRequest::stable_method_names();
    assert_eq!(methods.len(), 13);
    assert!(methods.contains(&"initialize"));
    assert!(methods.contains(&"thread/start"));
    assert!(methods.contains(&"thread/resume"));
    assert!(methods.contains(&"thread/read"));
    assert!(methods.contains(&"thread/list"));
    assert!(methods.contains(&"thread/archive"));
    assert!(methods.contains(&"turn/start"));
    assert!(methods.contains(&"turn/interrupt"));
    assert!(methods.contains(&"turn/steer"));
    assert!(methods.contains(&"approval/resolve"));
    assert!(methods.contains(&"model/list"));
    assert!(methods.contains(&"schema/export"));
    assert!(methods.contains(&"initialized"));
}

#[test]
fn stable_notifications_cover_thread_turn_item_and_approval() {
    let methods = ServerNotification::stable_method_names();
    assert!(methods.contains(&"thread/started"));
    assert!(methods.contains(&"thread/archived"));
    assert!(methods.contains(&"turn/started"));
    assert!(methods.contains(&"turn/completed"));
    assert!(methods.contains(&"item/started"));
    assert!(methods.contains(&"item/agentMessage/delta"));
    assert!(methods.contains(&"item/completed"));
    assert!(methods.contains(&"approval/requested"));
    assert!(methods.contains(&"approval/resolved"));
}

#[test]
fn stable_server_requests_include_approval_request() {
    let methods = ServerRequest::stable_method_names();
    assert_eq!(methods, vec!["approval/request"]);
}

#[test]
fn initialize_response_advertises_mvp_capabilities() {
    let response = InitializeResponse::new(
        "vv-agent-rs",
        env!("CARGO_PKG_VERSION"),
        AppServerCapabilities::mvp(),
    );

    assert_eq!(response.protocol_version, "2026-06-02");
    assert!(response.capabilities.thread);
    assert!(response.capabilities.turn);
    assert!(response.capabilities.item_stream);
    assert!(response.capabilities.approval_requests);
    assert!(response.supported_transports.contains(&"stdio".to_string()));
}

#[test]
fn initialize_params_use_camel_case_wire_shape() {
    let params = InitializeParams {
        client_info: AppClientInfo {
            name: "v_claw".to_string(),
            title: Some("v-claw".to_string()),
            version: Some("1.0.0".to_string()),
        },
        capabilities: AppClientCapabilities {
            experimental_api: false,
            opt_out_notification_methods: vec!["item/agentMessage/delta".to_string()],
        },
    };

    let value = serde_json::to_value(&params).expect("serialize");
    assert_eq!(value["clientInfo"]["name"], "v_claw");
    assert_eq!(value["capabilities"]["experimentalApi"], false);
    assert_eq!(
        value["capabilities"]["optOutNotificationMethods"][0],
        "item/agentMessage/delta"
    );
}

#[test]
fn client_request_serializes_with_stable_method_name() {
    let request = ClientRequest::ThreadStart(ThreadStartParams {
        cwd: Some("/tmp/project".into()),
        title: Some("Investigate".to_string()),
        model: Some("kimi-k2".to_string()),
        ephemeral: false,
    });

    let value = serde_json::to_value(&request).expect("serialize");
    assert_eq!(value["method"], "thread/start");
    assert_eq!(value["params"]["title"], "Investigate");
    assert_eq!(value["params"]["ephemeral"], false);
}

#[test]
fn server_request_serializes_approval_request_payload() {
    let request = ServerRequest::ApprovalRequest(ApprovalRequestParams {
        thread_id: "thread_1".to_string(),
        turn_id: "turn_1".to_string(),
        request_id: "approval_1".to_string(),
        tool_name: "bash".to_string(),
        preview: "Run cargo test".to_string(),
        choices: vec![ApprovalDecision::Allow, ApprovalDecision::Deny],
    });

    let value = serde_json::to_value(&request).expect("serialize");
    assert_eq!(value["method"], "approval/request");
    assert_eq!(value["params"]["threadId"], "thread_1");
    assert_eq!(value["params"]["choices"], json!(["allow", "deny"]));
}
