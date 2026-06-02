use vv_agent::app_server::protocol::{
    generate_app_server_json_schema_bundle, generate_app_server_typescript_bundle,
};

#[test]
fn json_schema_bundle_contains_protocol_envelopes() {
    let bundle = generate_app_server_json_schema_bundle().expect("schema bundle");
    assert!(bundle.contains_key("ClientRequest"));
    assert!(bundle.contains_key("ServerNotification"));
    assert!(bundle.contains_key("ServerRequest"));
    assert!(bundle.contains_key("JsonRpcMessage"));
}

#[test]
fn typescript_bundle_contains_generated_protocol_types() {
    let bundle = generate_app_server_typescript_bundle().expect("typescript bundle");
    assert!(bundle.contains_key("ClientRequest.ts"));
    assert!(bundle.contains_key("ServerNotification.ts"));
    assert!(bundle.contains_key("ServerRequest.ts"));
}
