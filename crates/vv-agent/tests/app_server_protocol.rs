use serde_json::json;
use vv_agent::app_server::protocol::{
    AppServerErrorCode, JsonRpcError, JsonRpcMessage, JsonRpcRequest, RequestId,
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
