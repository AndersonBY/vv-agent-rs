use serde_json::json;
use vv_agent::app_server::protocol::{
    AppClientCapabilities, AppClientInfo, AppServerCapabilities, AppServerErrorCode,
    ApprovalDecision, ApprovalRequestParams, ApprovalResolveParams, ClientRequest,
    InitializeParams, InitializeResponse, JsonRpcError, JsonRpcMessage, JsonRpcRequest, RequestId,
    ServerNotification, ServerRequest, ThreadStartParams, TurnResumeParams, TurnResumeResponse,
    TurnStatus,
};

#[test]
fn json_rpc_request_round_trips_with_jsonrpc_header() {
    let request = JsonRpcRequest {
        id: RequestId::Integer(1),
        method: "initialize".to_string(),
        params: Some(json!({"clientInfo": {"name": "test"}})),
    };

    let value = serde_json::to_value(&request).expect("serialize");
    assert_eq!(value["id"], 1);
    assert_eq!(value["method"], "initialize");
    assert_eq!(value["jsonrpc"], "2.0");

    let decoded: JsonRpcRequest = serde_json::from_value(value).expect("deserialize");
    assert_eq!(decoded.id, RequestId::Integer(1));
}

#[test]
fn json_rpc_message_decodes_request_notification_response_and_error() {
    let request: JsonRpcMessage =
        serde_json::from_value(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}))
            .expect("request");
    assert!(matches!(request, JsonRpcMessage::Request(_)));

    let notification: JsonRpcMessage =
        serde_json::from_value(json!({"jsonrpc": "2.0", "method": "initialized"}))
            .expect("notification");
    assert!(matches!(notification, JsonRpcMessage::Notification(_)));

    let response: JsonRpcMessage =
        serde_json::from_value(json!({"jsonrpc": "2.0", "id": 1, "result": {}})).expect("response");
    assert!(matches!(response, JsonRpcMessage::Response(_)));

    let error: JsonRpcMessage = serde_json::from_value(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "error": {"code": -32010, "message": "Not initialized"}
    }))
    .expect("error");
    assert!(matches!(error, JsonRpcMessage::Error(_)));
}

#[test]
fn json_rpc_message_rejects_unknown_top_level_fields() {
    for value in [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "extra": true}),
        json!({"jsonrpc": "2.0", "method": "initialized", "extra": true}),
        json!({"jsonrpc": "2.0", "id": 1, "result": {}, "extra": true}),
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {"code": -32603, "message": "failed"},
            "extra": true
        }),
    ] {
        assert!(serde_json::from_value::<JsonRpcMessage>(value).is_err());
    }
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
    assert_eq!(methods.len(), 16);
    assert!(methods.contains(&"initialize"));
    assert!(methods.contains(&"thread/start"));
    assert!(methods.contains(&"thread/resume"));
    assert!(methods.contains(&"thread/read"));
    assert!(methods.contains(&"thread/list"));
    assert!(methods.contains(&"thread/archive"));
    assert!(methods.contains(&"thread/unsubscribe"));
    assert!(methods.contains(&"turn/start"));
    assert!(methods.contains(&"turn/resume"));
    assert!(methods.contains(&"turn/interrupt"));
    assert!(methods.contains(&"turn/steer"));
    assert!(methods.contains(&"turn/followUp"));
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
    let response = InitializeResponse::new(AppServerCapabilities::for_runtime(true));

    assert_eq!(response.protocol_version, "v1");
    assert_eq!(response.user_agent, "vv-agent-app-server");
    assert!(response.capabilities.model_list);
    assert!(response.capabilities.thread_lifecycle);
    assert!(response.capabilities.notification_opt_out);
    assert!(response.capabilities.schema_export);
    assert!(response.capabilities.approval_resolve);
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
        agent_key: "assistant".to_string(),
        cwd: Some("/tmp/project".into()),
        metadata: std::collections::BTreeMap::from([(
            "source".to_string(),
            json!("protocol-test"),
        )]),
    });

    let value = serde_json::to_value(&request).expect("serialize");
    assert_eq!(value["method"], "thread/start");
    assert_eq!(value["params"]["agentKey"], "assistant");
    assert_eq!(value["params"]["metadata"]["source"], "protocol-test");
}

#[test]
fn turn_resume_wire_is_closed_and_has_no_input_field() {
    let params: TurnResumeParams = serde_json::from_value(json!({
        "threadId": "thread_1",
        "turnId": "turn_1",
        "checkpointKey": "tenant-7/run-42"
    }))
    .expect("turn resume params");
    let value = serde_json::to_value(params).expect("serialize params");
    let fields = value.as_object().expect("object");
    assert_eq!(fields.len(), 3);
    for field in ["threadId", "turnId", "checkpointKey"] {
        assert!(fields.contains_key(field));
    }
    assert!(serde_json::from_value::<TurnResumeParams>(json!({
        "threadId": "thread_1",
        "turnId": "turn_1",
        "checkpointKey": "tenant-7/run-42",
        "input": []
    }))
    .is_err());

    assert!(serde_json::from_value::<TurnResumeResponse>(json!({
        "threadId": "thread_1",
        "turnId": "turn_1",
        "runId": "run_1",
        "status": TurnStatus::Running,
        "runDefinition": {"secret": true}
    }))
    .is_err());
}

#[test]
fn server_request_serializes_approval_request_payload() {
    let request = ServerRequest::ApprovalRequest(ApprovalRequestParams {
        thread_id: "thread_1".to_string(),
        turn_id: "turn_1".to_string(),
        request_id: "approval_1".to_string(),
        tool_call_id: "call_1".to_string(),
        tool_name: "bash".to_string(),
        preview: "Run cargo test".to_string(),
        arguments: json!({"cmd": "cargo test"}),
    });

    let value = serde_json::to_value(&request).expect("serialize");
    assert_eq!(value["method"], "approval/request");
    assert_eq!(value["params"]["threadId"], "thread_1");
    assert_eq!(value["params"]["toolCallId"], "call_1");
    assert_eq!(value["params"]["arguments"], json!({"cmd": "cargo test"}));
    assert!(value["params"].get("choices").is_none());
}

#[test]
fn approval_decisions_round_trip_all_canonical_wire_values() {
    for (decision, expected) in [
        (ApprovalDecision::Allow, "allow"),
        (ApprovalDecision::AllowSession, "allow_session"),
        (ApprovalDecision::Deny, "deny"),
        (ApprovalDecision::Timeout, "timeout"),
    ] {
        let params = ApprovalResolveParams {
            thread_id: "thread_1".to_string(),
            turn_id: "turn_1".to_string(),
            request_id: "approval_1".to_string(),
            decision,
            reason: "approved by owner".to_string(),
            metadata: json!({"ticket": 7})
                .as_object()
                .expect("metadata object")
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        };
        let value = serde_json::to_value(&params).expect("serialize approval decision");
        assert_eq!(value["decision"], expected);
        assert_eq!(value["reason"], "approved by owner");
        assert_eq!(value["metadata"], json!({"ticket": 7}));
        let decoded: ApprovalResolveParams =
            serde_json::from_value(value).expect("deserialize approval decision");
        assert_eq!(decoded.decision, decision);
        assert_eq!(decoded.reason, "approved by owner");
        assert_eq!(decoded.metadata["ticket"], 7);
    }
}
