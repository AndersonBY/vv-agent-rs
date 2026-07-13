use vv_agent::app_server::protocol::{
    generate_app_server_json_schema_bundle, generate_app_server_typescript_bundle,
};

fn envelope_variants(schema: &str) -> Vec<serde_json::Value> {
    serde_json::from_str::<serde_json::Value>(schema)
        .expect("valid schema")
        .get("oneOf")
        .and_then(serde_json::Value::as_array)
        .expect("oneOf variants")
        .clone()
}

#[test]
fn json_schema_bundle_contains_protocol_envelopes() {
    let bundle = generate_app_server_json_schema_bundle().expect("schema bundle");
    assert!(bundle.contains_key("ClientRequest"));
    assert!(bundle.contains_key("ServerNotification"));
    assert!(bundle.contains_key("ServerRequest"));
    assert!(bundle.contains_key("JsonRpcMessage"));
    let approval = bundle
        .get("ApprovalDecision")
        .expect("approval decision schema");
    for decision in ["allow", "allow_session", "deny", "timeout"] {
        assert!(approval.contains(&format!("\"{decision}\"")));
    }
    let resolve: serde_json::Value = serde_json::from_str(
        bundle
            .get("ApprovalResolveParams")
            .expect("approval resolution schema"),
    )
    .expect("valid approval resolution schema");
    assert_eq!(resolve["properties"]["reason"]["type"], "string");
    assert_eq!(resolve["properties"]["metadata"]["type"], "object");
    assert!(!resolve["required"]
        .as_array()
        .expect("required fields")
        .iter()
        .any(|field| field == "reason" || field == "metadata"));
}

#[test]
fn typescript_bundle_contains_generated_protocol_types() {
    let bundle = generate_app_server_typescript_bundle().expect("typescript bundle");
    assert!(bundle.contains_key("ClientRequest.ts"));
    assert!(bundle.contains_key("ServerNotification.ts"));
    assert!(bundle.contains_key("ServerRequest.ts"));
    assert!(bundle
        .get("ApprovalDecision.ts")
        .expect("approval decision TypeScript")
        .contains("allow_session"));
}

#[test]
fn request_schemas_are_real_json_rpc_envelopes() {
    let bundle = generate_app_server_json_schema_bundle().expect("schema bundle");

    for variant in envelope_variants(bundle.get("ClientRequest").expect("client schema")) {
        let properties = variant["properties"].as_object().expect("properties");
        let required = variant["required"].as_array().expect("required");
        assert_eq!(properties["jsonrpc"]["const"], "2.0");
        assert!(required.iter().any(|field| field == "jsonrpc"));
        let method = properties["method"]["const"].as_str().expect("method");
        if method == "initialized" {
            assert!(!properties.contains_key("id"));
        } else {
            assert!(properties.contains_key("id"), "missing id for {method}");
            assert!(required.iter().any(|field| field == "id"));
        }
    }

    for variant in envelope_variants(bundle.get("ServerRequest").expect("server schema")) {
        let properties = variant["properties"].as_object().expect("properties");
        let required = variant["required"].as_array().expect("required");
        assert_eq!(properties["jsonrpc"]["const"], "2.0");
        assert!(properties.contains_key("id"));
        assert!(required.iter().any(|field| field == "jsonrpc"));
        assert!(required.iter().any(|field| field == "id"));
    }
}

#[test]
fn request_typescript_declares_json_rpc_envelopes() {
    let bundle = generate_app_server_typescript_bundle().expect("typescript bundle");
    let client = bundle.get("ClientRequest.ts").expect("client TypeScript");
    let server = bundle.get("ServerRequest.ts").expect("server TypeScript");
    assert!(client.contains("jsonrpc: \"2.0\""));
    assert!(client.contains("id: RequestId"));
    assert!(server.contains("jsonrpc: \"2.0\""));
    assert!(server.contains("id: RequestId"));
}
