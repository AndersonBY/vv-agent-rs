use serde_json::json;
use vv_agent::ToolOutput;

#[test]
fn tool_output_variants_preserve_shared_metadata_contract() {
    let text = ToolOutput::text("hello")
        .with_metadata("source", json!("test"))
        .to_result("text");
    assert_eq!(text.metadata["source"], "test");

    let data = ToolOutput::json(json!({"ok": true}))
        .with_metadata("source", json!("test"))
        .to_result("json");
    assert_eq!(data.metadata["output_type"], "json");
    assert_eq!(data.metadata["source"], "test");

    let error = ToolOutput::error("temporary failure")
        .with_code("temporary")
        .retryable(true)
        .with_metadata("source", json!("test"))
        .to_result("error");
    assert_eq!(error.error_code.as_deref(), Some("temporary"));
    assert_eq!(error.metadata["output_type"], "error");
    assert_eq!(error.metadata["retryable"], true);
    assert_eq!(error.metadata["source"], "test");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&error.content).expect("error JSON"),
        json!({
            "ok": false,
            "error": "temporary failure",
            "error_code": "temporary",
            "retryable": true,
        })
    );
}
