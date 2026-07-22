use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use vv_agent::prompt::{build_system_prompt_with_options, BuildSystemPromptOptions};
use vv_agent::types::AgentTask;
use vv_agent::{
    background_session_manager, build_default_registry, ToolCall, ToolContext, ToolExecutionResult,
    ToolExposure, ToolRegistry, ToolSpec,
};

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/parity/builtin_tool_behavior.json"))
        .expect("builtin tool behavior fixture")
}

fn arguments(value: &Value) -> BTreeMap<String, Value> {
    value
        .as_object()
        .expect("fixture arguments object")
        .clone()
        .into_iter()
        .collect()
}

fn execute(
    registry: &ToolRegistry,
    context: &mut ToolContext,
    tool_name: &str,
    arguments_value: &Value,
) -> ToolExecutionResult {
    registry
        .execute(
            &ToolCall::new(
                format!("fixture-{tool_name}"),
                tool_name,
                arguments(arguments_value),
            ),
            context,
        )
        .expect("fixture tool execution")
}

fn assert_result(result: &ToolExecutionResult, expected: &Value, contract: &Value) {
    let wire = result.to_dict();
    for key in ["status_code", "directive"] {
        assert_eq!(wire[key], expected[key], "outer result field {key}");
    }
    assert_eq!(wire.get("error_code"), expected.get("error_code"));
    let content: Value = serde_json::from_str(&result.content).expect("tool content JSON");
    assert_eq!(content, expected["content"]);
    assert_eq!(json!(result.metadata), expected["metadata"]);

    if expected["status_code"] == "ERROR" {
        for key in contract["error_content_required_keys"]
            .as_array()
            .expect("required error keys")
        {
            assert!(content.get(key.as_str().expect("error key")).is_some());
        }
    }
    for key in contract["metadata_policy"]["forbidden_large_keys"]
        .as_array()
        .expect("forbidden metadata keys")
    {
        assert!(
            !result
                .metadata
                .contains_key(key.as_str().expect("metadata key")),
            "metadata must not repeat bulk field {key}"
        );
    }
}

#[test]
fn fixture_drives_schema_validation_before_handler_execution() {
    let fixture = fixture();
    let contract = &fixture["canonical"];
    let case = &fixture["tools"]["schema_validation"];
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let mut registry = ToolRegistry::new();
    registry
        .register_tool_with_parameters(
            "schema_validation",
            "Validate fixture arguments.",
            case["schema"].clone(),
            Arc::new(move |_context, _arguments| {
                observed.fetch_add(1, Ordering::SeqCst);
                ToolExecutionResult::success("", "{}")
            }),
        )
        .expect("register validation tool");
    let workspace = tempfile::tempdir().expect("workspace");
    let mut context = ToolContext::new(workspace.path());

    let result = execute(
        &registry,
        &mut context,
        "schema_validation",
        &case["invalid_arguments"],
    );

    assert_result(&result, &case["result"], contract);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn fixture_drives_prompt_registry_dynamic_hint_and_projection() {
    let fixture = fixture();
    let empty_skills = &fixture["prompt"]["empty_skills"];
    let prompt = build_system_prompt_with_options(
        "Fixture agent",
        BuildSystemPromptOptions {
            available_skills: Some(empty_skills["available_skills"].clone()),
            current_time_utc: Some("2026-07-12T00:00:00Z".to_string()),
            ..BuildSystemPromptOptions::default()
        },
    );
    for fragment in empty_skills["forbidden_fragments"]
        .as_array()
        .expect("forbidden prompt fragments")
    {
        assert!(!prompt.contains(fragment.as_str().expect("prompt fragment")));
    }

    let mut registry = build_default_registry();
    let description_case = &fixture["registry"]["builtin_description"];
    let spec = registry
        .get(description_case["tool_name"].as_str().expect("tool name"))
        .expect("builtin spec");
    assert_eq!(
        !spec.description.trim().is_empty(),
        description_case["must_be_non_empty"]
            .as_bool()
            .expect("flag")
    );
    assert_eq!(
        spec.description,
        spec.schema["function"]["description"]
            .as_str()
            .expect("schema description")
    );

    let hidden_case = &fixture["registry"]["hidden_exposure"];
    let hidden_name = hidden_case["tool_name"].as_str().expect("hidden tool name");
    let mut hidden = ToolSpec::new(
        hidden_name,
        "Fixture hidden tool",
        Arc::new(|_, _| ToolExecutionResult::success("", "{}")),
    );
    hidden.exposure = ToolExposure::Hidden;
    registry.register(hidden).expect("register hidden tool");
    let visible_names = registry
        .list_openai_schemas(None)
        .expect("model-visible schemas")
        .into_iter()
        .filter_map(|schema| schema["function"]["name"].as_str().map(str::to_string))
        .collect::<Vec<_>>();
    assert_eq!(
        visible_names.iter().any(|name| name == hidden_name),
        hidden_case["must_be_model_visible"]
            .as_bool()
            .expect("flag")
    );

    let hint_case = &fixture["dynamic_bash_description"]["non_string_bash_shell"];
    let mut task = AgentTask::new("fixture-task", "fixture-model", "system", "user");
    task.agent_type = Some("computer".to_string());
    task.metadata
        .insert("bash_shell".to_string(), hint_case["bash_shell"].clone());
    let bash_schema = registry
        .planned_openai_schemas(&task)
        .into_iter()
        .find(|schema| schema["function"]["name"] == "bash")
        .expect("bash schema");
    assert!(bash_schema["function"]["description"]
        .as_str()
        .expect("description")
        .contains(hint_case["expected_hint"].as_str().expect("expected hint")));

    let projection = &fixture["tool_execution_result_projection"];
    let result = ToolExecutionResult::from_dict(&projection["canonical"]).expect("projection");
    assert_eq!(result.to_dict(), projection["canonical"]);
    assert_eq!(
        serde_json::to_value(&result).expect("raw serde projection"),
        projection["canonical"]
    );
    let round_trip: ToolExecutionResult =
        serde_json::from_value(projection["canonical"].clone()).expect("serde round trip");
    assert_eq!(round_trip, result);
    for field in projection["forbidden_absent_fields"]
        .as_array()
        .expect("absent fields")
    {
        assert!(serde_json::to_value(&result)
            .expect("serialized result")
            .get(field.as_str().expect("field"))
            .is_none());
    }
}

#[test]
fn fixture_drives_builtin_handler_envelopes_and_metadata() {
    let fixture = fixture();
    let contract = &fixture["canonical"];
    let tools = &fixture["tools"];
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();

    let mut context = ToolContext::new(workspace.path());
    let case = &tools["compress_memory"]["success"];
    let result = execute(
        &registry,
        &mut context,
        "compress_memory",
        &case["arguments"],
    );
    assert_result(&result, &case["result"], contract);
    let case = &tools["compress_memory"]["missing_core_information"];
    let result = execute(
        &registry,
        &mut context,
        "compress_memory",
        &case["arguments"],
    );
    assert_result(&result, &case["result"], contract);

    let skill_case = &tools["activate_skill"]["success"];
    let mut context = ToolContext::new(workspace.path());
    context.shared_state.insert(
        "available_skills".to_string(),
        skill_case["available_skills"].clone(),
    );
    let result = execute(
        &registry,
        &mut context,
        "activate_skill",
        &skill_case["arguments"],
    );
    assert_result(&result, &skill_case["result"], contract);

    let mut context = ToolContext::new(workspace.path());
    context.metadata.insert(
        "available_skills".to_string(),
        skill_case["available_skills"].clone(),
    );
    let result = execute(
        &registry,
        &mut context,
        "activate_skill",
        &skill_case["arguments"],
    );
    assert_result(
        &result,
        &tools["activate_skill"]["metadata_only_source"]["result"],
        contract,
    );

    let image_case = &tools["read_image"]["too_large"];
    std::fs::write(
        workspace
            .path()
            .join(image_case["path"].as_str().expect("image path")),
        vec![b'x'; image_case["actual_bytes"].as_u64().expect("image size") as usize],
    )
    .expect("large image");
    let mut context = ToolContext::new(workspace.path());
    let result = execute(
        &registry,
        &mut context,
        "read_image",
        &json!({"path": image_case["path"]}),
    );
    assert_result(&result, &image_case["result"], contract);

    for (tool_name, case) in [
        ("file_info", &tools["file_info"]["missing_path"]),
        ("find_files", &tools["find_files"]["missing_directory"]),
        ("task_finish", &tools["control"]["blank_task_finish"]),
        ("ask_user", &tools["control"]["blank_ask_user"]),
    ] {
        let mut context = ToolContext::new(workspace.path());
        let result = execute(&registry, &mut context, tool_name, &case["arguments"]);
        assert_result(&result, &case["result"], contract);
    }
}

#[test]
fn fixture_drives_bash_and_background_command_contract() {
    let fixture = fixture();
    let contract = &fixture["canonical"];
    let tools = &fixture["tools"];
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();

    let non_zero = &tools["bash"]["non_zero"];
    let mut context = ToolContext::new(workspace.path());
    for (key, value) in non_zero["context_metadata"]
        .as_object()
        .expect("context metadata")
    {
        context.metadata.insert(key.clone(), value.clone());
    }
    let result = execute(&registry, &mut context, "bash", &non_zero["arguments"]);
    assert_result(&result, &non_zero["result"], contract);

    let invalid_timeout = &tools["bash"]["invalid_timeout"];
    let mut context = ToolContext::new(workspace.path());
    let result = execute(
        &registry,
        &mut context,
        "bash",
        &invalid_timeout["arguments"],
    );
    assert_result(&result, &invalid_timeout["result"], contract);

    let background = &tools["bash"]["background_start"];
    let mut context = ToolContext::new(workspace.path());
    for (key, value) in background["context_metadata"]
        .as_object()
        .expect("context metadata")
    {
        context.metadata.insert(key.clone(), value.clone());
    }
    let result = execute(&registry, &mut context, "bash", &background["arguments"]);
    let expected = &background["result"];
    let wire = result.to_dict();
    for key in ["status", "status_code", "directive"] {
        assert_eq!(wire[key], expected[key]);
    }
    let content: Value = serde_json::from_str(&result.content).expect("background content");
    for (key, value) in expected["content_subset"]
        .as_object()
        .expect("content subset")
    {
        assert_eq!(&content[key], value);
    }
    for (key, value) in expected["metadata_subset"]
        .as_object()
        .expect("metadata subset")
    {
        assert_eq!(result.metadata.get(key), Some(value));
    }
    for key in expected["required_dynamic_fields"]
        .as_array()
        .expect("dynamic fields")
    {
        let key = key.as_str().expect("dynamic key");
        assert!(content[key].as_str().is_some_and(|value| !value.is_empty()));
        assert_eq!(result.metadata.get(key), content.get(key));
    }
    for key in contract["metadata_policy"]["forbidden_large_keys"]
        .as_array()
        .expect("forbidden metadata keys")
    {
        assert!(!result
            .metadata
            .contains_key(key.as_str().expect("metadata key")));
    }

    let session_id = content["session_id"].as_str().expect("session id");
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let payload = background_session_manager().check(session_id);
        if payload["status"] != "running" {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }

    let missing = &tools["check_background_command"]["missing_session"];
    let mut context = ToolContext::new(workspace.path());
    let result = execute(
        &registry,
        &mut context,
        "check_background_command",
        &missing["arguments"],
    );
    assert_result(&result, &missing["result"], contract);
}
