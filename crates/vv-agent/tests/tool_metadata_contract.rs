use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};
use vv_agent::{
    FunctionTool, Tool, ToolCall, ToolContext, ToolDirective, ToolLifecycleEvent, ToolMetadata,
    ToolOrchestrator, ToolOutput, ToolPolicy, ToolRegistry, ToolResultStatus, ToolRunOptions,
    ToolSideEffect,
};

const CONTRACT_SOURCE: &str = include_str!("fixtures/parity/tool_metadata.json");
const TOOL_NAME: &str = "fixture_tool";

fn contract() -> Value {
    serde_json::from_str(CONTRACT_SOURCE).expect("valid tool metadata parity fixture")
}

fn build_tool(tool_metadata: Option<ToolMetadata>) -> Result<FunctionTool<Value>, String> {
    let builder =
        FunctionTool::builder(TOOL_NAME).description("Fixture-backed tool metadata producer.");
    let builder = match tool_metadata {
        Some(tool_metadata) => builder.tool_metadata(tool_metadata),
        None => builder,
    };
    builder
        .handler(|_context, _arguments: Value| async { Ok(ToolOutput::text("ok")) })
        .build()
}

fn serialized_tool_metadata(tool: &impl Tool) -> Value {
    tool.tool_metadata().map_or(Value::Null, |metadata| {
        serde_json::to_value(metadata).expect("tool metadata serializes")
    })
}

fn generated_labels(generator: &Value) -> Vec<String> {
    if let Some(count) = generator["count"].as_u64() {
        let prefix = generator["value_prefix"].as_str().expect("value prefix");
        return (0..count).map(|index| format!("{prefix}{index}")).collect();
    }

    let value = generator["value"].as_str().expect("generated value");
    let code_points = generator["code_points"].as_u64().expect("code points");
    vec![value.repeat(code_points as usize)]
}

fn policy_from_fixture(value: &Value) -> ToolPolicy {
    let mut policy = ToolPolicy::default();
    if let Some(denied_side_effects) = value.get("denied_side_effects") {
        policy.denied_side_effects =
            serde_json::from_value::<Vec<ToolSideEffect>>(denied_side_effects.clone())
                .expect("valid denied side effects");
    }
    if let Some(denied_capability_tags) = value.get("denied_capability_tags") {
        policy.denied_capability_tags = serde_json::from_value(denied_capability_tags.clone())
            .expect("valid denied capability tags");
    }
    if let Some(deny_terminal_tools) = value.get("deny_terminal_tools") {
        policy.deny_terminal_tools = deny_terminal_tools.as_bool().expect("terminal denial flag");
    }
    if let Some(denied_cost_dimensions) = value.get("denied_cost_dimensions") {
        policy.denied_cost_dimensions = serde_json::from_value(denied_cost_dimensions.clone())
            .expect("valid denied cost dimensions");
    }
    policy.normalized().expect("fixture policy normalizes")
}

fn assert_schema_has_no_keys(value: &Value, forbidden: &BTreeSet<String>) {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                assert!(
                    !forbidden.contains(key),
                    "model-visible schema leaked `{key}`: {value}"
                );
                assert_schema_has_no_keys(child, forbidden);
            }
        }
        Value::Array(values) => {
            for child in values {
                assert_schema_has_no_keys(child, forbidden);
            }
        }
        _ => {}
    }
}

fn lifecycle_event_type(event: &ToolLifecycleEvent) -> &'static str {
    match event {
        ToolLifecycleEvent::Planned { .. } => "tool_call_planned",
        ToolLifecycleEvent::Started { .. } => "tool_call_started",
        ToolLifecycleEvent::Completed { .. } => "tool_call_completed",
    }
}

fn lifecycle_event_value(event: &ToolLifecycleEvent) -> Value {
    let (event_type, call, tool_metadata) = match event {
        ToolLifecycleEvent::Planned {
            call,
            tool_metadata,
        } => ("tool_call_planned", call, tool_metadata),
        ToolLifecycleEvent::Started {
            call,
            tool_metadata,
        } => ("tool_call_started", call, tool_metadata),
        ToolLifecycleEvent::Completed {
            call,
            tool_metadata,
            ..
        } => ("tool_call_completed", call, tool_metadata),
    };
    let mut value = json!({
        "type": event_type,
        "tool_name": call.name,
        "tool_call_id": call.id,
    });
    if !matches!(event, ToolLifecycleEvent::Completed { .. }) {
        value["arguments"] = serde_json::to_value(&call.arguments).expect("arguments serialize");
    }
    if let Some(tool_metadata) = tool_metadata {
        value["tool_metadata"] =
            serde_json::to_value(tool_metadata).expect("lifecycle metadata serializes");
    }

    if let ToolLifecycleEvent::Completed {
        result,
        execution_started,
        duration_ms,
        ..
    } = event
    {
        value["status"] = Value::String(
            serde_json::to_value(result.status)
                .expect("status serializes")
                .as_str()
                .expect("status string")
                .to_ascii_lowercase(),
        );
        value["directive"] = serde_json::to_value(result.directive).expect("directive serializes");
        value["error_code"] = serde_json::to_value(&result.error_code).expect("error serializes");
        value["execution_started"] = Value::Bool(*execution_started);
        value["duration_ms"] = serde_json::to_value(duration_ms).expect("duration serializes");
    }
    value
}

#[test]
fn public_tool_metadata_serde_and_function_builder_consume_fixture_cases() {
    let contract = contract();

    for case in contract["normalization_cases"]
        .as_array()
        .expect("normalization cases")
    {
        let name = case["name"].as_str().expect("case name");
        let metadata: ToolMetadata = serde_json::from_value(case["input"].clone())
            .unwrap_or_else(|error| panic!("{name}: metadata must deserialize: {error}"));
        let tool = build_tool(Some(metadata))
            .unwrap_or_else(|error| panic!("{name}: tool must build: {error}"));

        assert_eq!(serialized_tool_metadata(&tool), case["expected"], "{name}");
    }

    for case in contract["invalid_cases"].as_array().expect("invalid cases") {
        let name = case["name"].as_str().expect("case name");
        assert!(
            serde_json::from_value::<ToolMetadata>(case["input"].clone()).is_err(),
            "{name}: invalid metadata must be rejected"
        );
    }

    for case in contract["generated_invalid_cases"]
        .as_array()
        .expect("generated invalid cases")
    {
        let name = case["name"].as_str().expect("case name");
        let generator = &case["generator"];
        let field = generator["field"].as_str().expect("generated field");
        let values = generated_labels(generator);

        if field.starts_with("denied_") {
            let mut policy = ToolPolicy::default();
            match field {
                "denied_capability_tags" => policy.denied_capability_tags = values,
                "denied_cost_dimensions" => policy.denied_cost_dimensions = values,
                _ => panic!("{name}: unsupported generated policy field `{field}`"),
            }
            assert!(
                policy.normalized().is_err(),
                "{name}: invalid policy labels must be rejected"
            );
        } else {
            let mut input = Map::new();
            input.insert(
                field.to_string(),
                Value::Array(values.into_iter().map(Value::String).collect()),
            );
            assert!(
                serde_json::from_value::<ToolMetadata>(Value::Object(input)).is_err(),
                "{name}: invalid generated metadata must be rejected"
            );
        }
    }
}

#[tokio::test]
async fn tool_policy_producer_consumes_fixture_policy_cases() {
    let contract = contract();

    for case in contract["policy_cases"].as_array().expect("policy cases") {
        let name = case["name"].as_str().expect("case name");
        let tool_metadata: Option<ToolMetadata> = serde_json::from_value(case["metadata"].clone())
            .unwrap_or_else(|error| panic!("{name}: tool metadata: {error}"));
        let invocations = Arc::new(AtomicUsize::new(0));
        let handler_invocations = Arc::clone(&invocations);
        let builder =
            FunctionTool::builder(TOOL_NAME).description("Fixture-backed tool policy producer.");
        let builder = match tool_metadata {
            Some(tool_metadata) => builder.tool_metadata(tool_metadata),
            None => builder,
        };
        let tool = builder
            .handler(move |_context, _arguments: Value| {
                let handler_invocations = Arc::clone(&handler_invocations);
                async move {
                    handler_invocations.fetch_add(1, Ordering::SeqCst);
                    Ok(ToolOutput::text("ok"))
                }
            })
            .build()
            .unwrap_or_else(|error| panic!("{name}: tool must build: {error}"));

        let mut policy = policy_from_fixture(&case["policy"]);
        if case.get("existing_name_policy_allows") == Some(&Value::Bool(false)) {
            policy = policy.disallow(TOOL_NAME);
        }
        let orchestrator = ToolOrchestrator::from_tools(vec![tool.to_executor()]);
        let mut context = ToolContext::new(".");
        let result = orchestrator
            .run_one(
                ToolCall::from_raw_arguments("fixture-call", TOOL_NAME, json!({})),
                &mut context,
                ToolRunOptions::from_policy(&policy),
            )
            .await
            .unwrap_or_else(|error| panic!("{name}: orchestrator failed: {error}"));

        if case["allowed"].as_bool().expect("allowed flag") {
            assert_eq!(invocations.load(Ordering::SeqCst), 1, "{name}");
            assert_eq!(result.status, ToolResultStatus::Success, "{name}");
            assert!(!result.metadata.contains_key("policy_source"), "{name}");
        } else {
            assert_eq!(invocations.load(Ordering::SeqCst), 0, "{name}");
            assert_eq!(result.status, ToolResultStatus::Error, "{name}");
            assert_eq!(
                result.error_code.as_deref(),
                Some("tool_not_allowed"),
                "{name}"
            );
            assert_eq!(
                result.metadata.get("policy_source"),
                Some(&case["policy_source"]),
                "{name}"
            );
        }
    }
}

#[test]
fn generic_metadata_stays_separate_and_typed_metadata_is_not_model_visible() {
    let contract = contract();
    assert_eq!(
        contract["metadata_contract"]["generic_metadata_is_not_a_declaration"],
        Value::Bool(true)
    );
    assert_eq!(
        contract["metadata_contract"]["model_visible"],
        Value::Bool(false)
    );
    assert_eq!(
        contract["public_construction"]["generic_metadata_remains_separate"],
        Value::Bool(true)
    );

    let declaration = contract["normalization_cases"][0]["expected"].clone();
    let typed_metadata: ToolMetadata =
        serde_json::from_value(declaration.clone()).expect("canonical declaration");

    let mut generic_builder = FunctionTool::builder("generic_fixture_tool")
        .description("Generic metadata remains separate.");
    for (key, value) in declaration.as_object().expect("metadata object") {
        generic_builder = generic_builder.metadata(key, value.clone());
    }
    generic_builder = generic_builder.metadata("tool_metadata", declaration.clone());
    let generic_tool = generic_builder
        .handler(|_context, _arguments: Value| async { Ok(ToolOutput::text("ok")) })
        .build()
        .expect("generic metadata tool");
    let typed_tool = FunctionTool::builder("typed_fixture_tool")
        .description("Typed metadata remains host-visible.")
        .tool_metadata(typed_metadata)
        .handler(|_context, _arguments: Value| async { Ok(ToolOutput::text("ok")) })
        .build()
        .expect("typed metadata tool");

    let generic_spec = generic_tool.as_tool_spec();
    let typed_spec = typed_tool.as_tool_spec();
    assert!(generic_spec.tool_metadata.is_none());
    assert_eq!(
        generic_spec.metadata.get("tool_metadata"),
        Some(&declaration)
    );
    assert_eq!(
        serde_json::to_value(
            typed_spec
                .tool_metadata
                .as_ref()
                .expect("typed declaration")
        )
        .expect("typed metadata serializes"),
        declaration
    );
    assert!(typed_spec.metadata.is_empty());

    let mut registry = ToolRegistry::new();
    registry
        .register(generic_spec)
        .expect("register generic tool");
    registry.register(typed_spec).expect("register typed tool");
    let schemas = registry
        .list_openai_schemas(None)
        .expect("model-visible schemas");
    let forbidden = contract["metadata_contract"]["closed_fields"]
        .as_array()
        .expect("closed metadata fields")
        .iter()
        .map(|field| field.as_str().expect("field name").to_string())
        .chain(["tool_metadata".to_string()])
        .collect::<BTreeSet<_>>();
    for schema in schemas {
        assert_schema_has_no_keys(&schema, &forbidden);
    }
}

#[test]
fn telemetry_contract_matches_public_status_and_directive_values() {
    let contract = contract();
    let telemetry = &contract["telemetry_contract"];
    assert_eq!(
        telemetry["event_types"],
        json!([
            "tool_call_planned",
            "tool_call_started",
            "tool_call_completed"
        ])
    );

    let status_values = [
        ToolResultStatus::Success,
        ToolResultStatus::Error,
        ToolResultStatus::WaitResponse,
        ToolResultStatus::Running,
        ToolResultStatus::PendingCompress,
    ]
    .into_iter()
    .map(|status| {
        Value::String(
            serde_json::to_value(status)
                .expect("status serializes")
                .as_str()
                .expect("status string")
                .to_ascii_lowercase(),
        )
    })
    .collect::<Vec<_>>();
    assert_eq!(telemetry["tool_status_values"], Value::Array(status_values));

    let directive_values = [
        ToolDirective::Continue,
        ToolDirective::WaitUser,
        ToolDirective::Finish,
    ]
    .into_iter()
    .map(|directive| {
        serde_json::to_value(directive)
            .expect("directive serializes")
            .as_str()
            .expect("directive string")
            .to_string()
    })
    .collect::<BTreeSet<_>>();
    let expected_directives = telemetry["directive_values"]
        .as_array()
        .expect("directive values")
        .iter()
        .map(|value| value.as_str().expect("directive string").to_string())
        .collect::<BTreeSet<_>>();
    assert_eq!(directive_values, expected_directives);
    assert_eq!(
        telemetry["parse_failure_before_planning_has_no_tool_lifecycle"],
        Value::Bool(true)
    );
    assert_eq!(telemetry["missing_metadata_field"], "omit");
    assert_eq!(
        telemetry["telemetry_changes_runtime_decisions"],
        Value::Bool(false)
    );
}

#[tokio::test]
async fn real_orchestrator_consumes_canonical_producer_cases() {
    let contract = contract();
    let telemetry = &contract["telemetry_contract"];

    for case in contract["producer_cases"]
        .as_array()
        .expect("producer cases")
    {
        let name = case["name"].as_str().expect("producer case name");
        let expected_events = case.get("expected_events").and_then(Value::as_array);
        let first_expected = expected_events.and_then(|events| events.first());
        let tool_name = first_expected
            .and_then(|event| event["tool_name"].as_str())
            .unwrap_or(TOOL_NAME);
        let tool_call_id = first_expected
            .and_then(|event| event["tool_call_id"].as_str())
            .map(str::to_string)
            .unwrap_or_else(|| format!("call_{name}"));
        let arguments = first_expected
            .map(|event| event["arguments"].clone())
            .unwrap_or_else(|| json!({}));
        let tool_metadata: Option<ToolMetadata> =
            serde_json::from_value(case["tool_metadata"].clone())
                .unwrap_or_else(|error| panic!("{name}: metadata: {error}"));
        let invocations = Arc::new(AtomicUsize::new(0));
        let handler_invocations = Arc::clone(&invocations);
        let builder = FunctionTool::builder(tool_name)
            .description("Canonical telemetry producer.")
            .json_schema(json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                }
            }));
        let builder = match tool_metadata {
            Some(tool_metadata) => builder.tool_metadata(tool_metadata),
            None => builder,
        };
        let tool = builder
            .handler(move |_context, _arguments: Value| {
                let handler_invocations = Arc::clone(&handler_invocations);
                async move {
                    handler_invocations.fetch_add(1, Ordering::SeqCst);
                    Ok(ToolOutput::text("ok"))
                }
            })
            .build()
            .unwrap_or_else(|error| panic!("{name}: tool: {error}"));

        let events = Arc::new(Mutex::new(Vec::<ToolLifecycleEvent>::new()));
        let callback_events = Arc::clone(&events);
        let callback = Arc::new(move |event| {
            callback_events
                .lock()
                .expect("lifecycle event lock")
                .push(event);
        });
        let policy = policy_from_fixture(&case.get("policy").cloned().unwrap_or_else(|| json!({})));
        let options = ToolRunOptions::from_policy(&policy).lifecycle_callback(callback);
        let result = ToolOrchestrator::from_tools(vec![tool.to_executor()])
            .run_one(
                ToolCall::from_raw_arguments(&tool_call_id, tool_name, arguments.clone()),
                &mut ToolContext::new("."),
                options,
            )
            .await
            .unwrap_or_else(|error| panic!("{name}: orchestrator: {error}"));
        let captured = events.lock().expect("captured lifecycle events");
        let event_types = captured
            .iter()
            .map(lifecycle_event_type)
            .collect::<Vec<_>>();

        if let Some(expected_events) = expected_events {
            let mut actual_events = captured
                .iter()
                .map(lifecycle_event_value)
                .collect::<Vec<_>>();
            let completed_duration = actual_events
                .last()
                .and_then(|event| event["duration_ms"].as_u64());
            assert!(completed_duration.is_some(), "{name}: execution duration");
            actual_events.last_mut().expect("completed event")["duration_ms"] =
                expected_events.last().expect("expected completed")["duration_ms"].clone();
            assert_eq!(actual_events, *expected_events, "{name}");
            assert_eq!(invocations.load(Ordering::SeqCst), 1, "{name}");
            assert_eq!(result.status, ToolResultStatus::Success, "{name}");
            continue;
        }

        assert_eq!(json!(event_types), case["expected_event_types"], "{name}");
        if name == "metadata_policy_denial_has_no_execution_start" {
            let completed = lifecycle_event_value(captured.last().expect("completed event"));
            for (field, expected) in case["expected_completed"]
                .as_object()
                .expect("expected completed")
            {
                assert_eq!(completed.get(field), Some(expected), "{name}: {field}");
            }
            assert_eq!(invocations.load(Ordering::SeqCst), 0, "{name}");
            assert_eq!(
                result.error_code.as_deref(),
                Some("tool_not_allowed"),
                "{name}"
            );
            continue;
        }

        assert_eq!(case["typed_metadata_field_present"], Value::Bool(false));
        assert!(
            captured
                .iter()
                .map(lifecycle_event_value)
                .all(|event| event.get("tool_metadata").is_none()),
            "{name}"
        );
        assert_eq!(invocations.load(Ordering::SeqCst), 1, "{name}");
        assert_eq!(result.status, ToolResultStatus::Success, "{name}");
    }

    let parse_events = Arc::new(Mutex::new(Vec::<ToolLifecycleEvent>::new()));
    let callback_events = Arc::clone(&parse_events);
    let parse_result = ToolOrchestrator::default()
        .run_one(
            ToolCall::from_raw_arguments("call_invalid", "missing", Value::String("{".into())),
            &mut ToolContext::new("."),
            ToolRunOptions::default().lifecycle_callback(Arc::new(move |event| {
                callback_events
                    .lock()
                    .expect("parse lifecycle lock")
                    .push(event);
            })),
        )
        .await
        .expect("parse failures are tool results");
    assert_eq!(
        telemetry["parse_failure_before_planning_has_no_tool_lifecycle"],
        Value::Bool(true)
    );
    assert_eq!(
        parse_result.error_code.as_deref(),
        Some("invalid_arguments_json")
    );
    assert!(parse_events.lock().expect("parse events").is_empty());
}
