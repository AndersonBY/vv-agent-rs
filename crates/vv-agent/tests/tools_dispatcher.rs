use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};
use vv_agent::tools::{build_default_registry, dispatch_tool_call, ToolContext, ToolSpec};
use vv_agent::{ToolCall, ToolDirective, ToolExecutionResult, ToolRegistry, ToolResultStatus};

#[test]
fn dispatcher_returns_structured_errors_for_invalid_arguments_and_unknown_tools() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());

    let invalid_json =
        ToolCall::from_raw_arguments("bad_json", "task_finish", Value::String("{".to_string()));
    let result = dispatch_tool_call(&registry, &mut context, &invalid_json);
    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(result.tool_call_id, "bad_json");
    assert_eq!(result.error_code.as_deref(), Some("invalid_arguments_json"));
    assert!(result.content.contains("Invalid tool arguments JSON"));

    let invalid_payload = ToolCall::from_raw_arguments(
        "bad_payload",
        "task_finish",
        Value::String("[\"not\", \"object\"]".to_string()),
    );
    let result = dispatch_tool_call(&registry, &mut context, &invalid_payload);
    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(
        result.error_code.as_deref(),
        Some("invalid_arguments_payload")
    );

    let unknown = ToolCall::new("missing", "missing_tool", BTreeMap::new());
    let result = dispatch_tool_call(&registry, &mut context, &unknown);
    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(result.tool_call_id, "missing");
    assert_eq!(result.error_code.as_deref(), Some("tool_not_found"));
    assert!(result.content.contains("Unknown tool: missing_tool"));
}

#[test]
fn dispatcher_normalizes_tool_call_id_and_wait_response_status() {
    let workspace = tempfile::tempdir().expect("workspace");
    let mut registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());

    registry
        .register(ToolSpec::new(
            "pending_id",
            "returns a pending tool call id",
            Arc::new(|_context, _arguments| ToolExecutionResult {
                tool_call_id: "pending".to_string(),
                content: json!({"ok": true}).to_string(),
                status: ToolResultStatus::Success,
                directive: ToolDirective::Continue,
                error_code: None,
                metadata: BTreeMap::new(),
                image_url: None,
                image_path: None,
            }),
        ))
        .expect("register");
    let result = dispatch_tool_call(
        &registry,
        &mut context,
        &ToolCall::new("call_pending", "pending_id", BTreeMap::new()),
    );
    assert_eq!(result.tool_call_id, "call_pending");
    assert_eq!(result.status, ToolResultStatus::Success);

    registry
        .register(ToolSpec::new(
            "blank_id",
            "returns a blank tool call id",
            Arc::new(|_context, _arguments| ToolExecutionResult {
                tool_call_id: "   ".to_string(),
                content: json!({"ok": true}).to_string(),
                status: ToolResultStatus::Success,
                directive: ToolDirective::Continue,
                error_code: None,
                metadata: BTreeMap::new(),
                image_url: None,
                image_path: None,
            }),
        ))
        .expect("register blank id tool");
    let result = dispatch_tool_call(
        &registry,
        &mut context,
        &ToolCall::new("call_blank", "blank_id", BTreeMap::new()),
    );
    assert_eq!(result.tool_call_id, "call_blank");
    assert_eq!(result.status, ToolResultStatus::Success);

    let wait_call = ToolCall::from_raw_arguments(
        "ask",
        "ask_user",
        json!({"question": "Which path?", "options": ["a", "b"]}),
    );
    let result = dispatch_tool_call(&registry, &mut context, &wait_call);
    assert_eq!(result.tool_call_id, "ask");
    assert_eq!(result.directive, ToolDirective::WaitUser);
    assert_eq!(result.status, ToolResultStatus::WaitResponse);
}

#[test]
fn register_tool_with_parameters_creates_schema_and_handler() {
    let mut registry = ToolRegistry::new();
    registry
        .register_tool_with_parameters(
            "_echo",
            "Echo arguments back.",
            json!({
                "type": "object",
                "properties": {"msg": {"type": "string"}},
                "required": ["msg"],
            }),
            Arc::new(|_context, arguments| {
                ToolExecutionResult::success("", json!(arguments).to_string())
            }),
        )
        .expect("register tool");

    assert!(registry.has_tool("_echo"));
    assert!(registry.has_schema("_echo"));
    let schema = registry.get_schema("_echo").expect("schema");
    assert_eq!(schema["function"]["name"], "_echo");
    assert_eq!(schema["function"]["description"], "Echo arguments back.");
    assert_eq!(schema["function"]["parameters"]["required"], json!(["msg"]));

    let workspace = tempfile::tempdir().expect("workspace");
    let mut context = ToolContext::new(workspace.path());
    let result = registry
        .execute(
            &ToolCall::from_raw_arguments("_call", "_echo", json!({"msg": "hi"})),
            &mut context,
        )
        .expect("execute");
    assert_eq!(result.status, ToolResultStatus::Success);
    assert!(result.content.contains("\"msg\":\"hi\""));
}

#[test]
fn register_preserves_explicit_schema_registered_before_handler() {
    let mut registry = ToolRegistry::new();
    registry.register_schema(
        "_workflow_custom_run",
        json!({
            "type": "function",
            "function": {
                "name": "_workflow_custom_run",
                "description": "Run workflow via custom integration layer.",
                "parameters": {
                    "type": "object",
                    "properties": {"workflow": {"type": "string"}},
                    "required": ["workflow"],
                },
            },
        }),
    );

    registry
        .register(ToolSpec::new(
            "_workflow_custom_run",
            "fallback description",
            Arc::new(|_context, arguments| {
                ToolExecutionResult::success("", json!({"received": arguments}).to_string())
            }),
        ))
        .expect("register handler");

    assert!(registry.has_tool("_workflow_custom_run"));
    assert!(registry.has_schema("_workflow_custom_run"));
    let schema = registry
        .get_schema("_workflow_custom_run")
        .expect("registered schema");
    assert_eq!(
        schema["function"]["description"],
        "Run workflow via custom integration layer."
    );
    assert_eq!(
        schema["function"]["parameters"]["required"],
        json!(["workflow"])
    );

    let workspace = tempfile::tempdir().expect("workspace");
    let mut context = ToolContext::new(workspace.path());
    let result = registry
        .execute(
            &ToolCall::from_raw_arguments(
                "_call",
                "_workflow_custom_run",
                json!({"workflow": "wf_translate"}),
            ),
            &mut context,
        )
        .expect("execute");
    assert_eq!(result.status, ToolResultStatus::Success);
    assert!(result.content.contains("wf_translate"));
}

#[test]
fn list_openai_schemas_defaults_to_registered_tools_order() {
    let mut registry = ToolRegistry::new();
    registry.register_schema(
        "_schema_only",
        json!({
            "type": "function",
            "function": {
                "name": "_schema_only",
                "description": "Schema without a handler.",
                "parameters": {"type": "object", "properties": {}, "required": []},
            },
        }),
    );
    registry
        .register_tool(
            "_b_second",
            "Second registered tool.",
            Arc::new(|_context, _arguments| ToolExecutionResult::success("", "{}")),
        )
        .expect("register second");
    registry
        .register_tool(
            "_a_first",
            "First registered tool despite lexical order.",
            Arc::new(|_context, _arguments| ToolExecutionResult::success("", "{}")),
        )
        .expect("register first");

    let names = registry
        .list_openai_schemas(None)
        .expect("schemas")
        .iter()
        .filter_map(|schema| schema["function"]["name"].as_str().map(str::to_string))
        .collect::<Vec<_>>();

    assert_eq!(names, vec!["_b_second", "_a_first"]);
}

#[test]
fn list_openai_schemas_rejects_missing_schema() {
    let mut registry = ToolRegistry::new();
    registry
        .register(ToolSpec::new(
            "_handler_without_schema",
            "Handler registered without an explicit schema.",
            Arc::new(|_context, _arguments| ToolExecutionResult::success("", "{}")),
        ))
        .expect("register handler");

    registry.register_schema(
        "_schema_only",
        json!({
            "type": "function",
            "function": {
                "name": "_schema_only",
                "description": "Schema without a handler.",
                "parameters": {"type": "object", "properties": {}, "required": []},
            },
        }),
    );

    let names = vec![
        "_handler_without_schema".to_string(),
        "_missing_schema".to_string(),
    ];
    let error = registry
        .list_openai_schemas(Some(&names))
        .expect_err("missing explicit schema should fail");

    assert!(error.contains("Schema not registered: _missing_schema"));
}

#[test]
fn register_tool_keeps_agent_default_empty_object_schema() {
    let mut registry = ToolRegistry::new();
    registry
        .register_tool(
            "_noop",
            "No-op tool.",
            Arc::new(|_context, _arguments| ToolExecutionResult::success("", "{}")),
        )
        .expect("register tool");

    let schema = registry.get_schema("_noop").expect("schema");
    assert_eq!(
        schema["function"]["parameters"],
        json!({"type": "object", "properties": {}, "required": []})
    );
}
