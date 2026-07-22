use serde_json::{json, Map, Value};
use vv_agent::types::AgentTask;
use vv_agent::SubAgentConfig;

#[test]
fn validation_messages_and_codes_match_shared_fixture() {
    let fixture = super::contract();
    let empty_model = SubAgentConfig::new("  ", "Research")
        .validate()
        .expect_err("empty model");
    assert_eq!(
        empty_model.code(),
        fixture["validation"]["empty_model_error_code"]
    );
    assert_eq!(
        empty_model.message(),
        fixture["validation"]["empty_model_message"]
    );

    let mut empty_prompt = SubAgentConfig::new("child-model", "Research");
    empty_prompt.system_prompt = Some(" \n ".to_string());
    let empty_prompt = empty_prompt.validate().expect_err("empty system prompt");
    assert_eq!(
        empty_prompt.code(),
        fixture["validation"]["empty_system_prompt_error_code"]
    );
    assert_eq!(
        empty_prompt.message(),
        fixture["validation"]["empty_system_prompt_message"]
    );

    let normalized = SubAgentConfig::new(
        fixture["validation"]["normalized_model_input"]
            .as_str()
            .expect("normalized model input"),
        "Research",
    );
    assert_eq!(
        normalized.model,
        fixture["validation"]["normalized_model_value"]
    );
}

#[test]
fn configured_fields_use_fixture_portable_whitespace_semantics() {
    let fixture = super::contract();
    let whitespace = &fixture["validation"]["portable_whitespace"];
    let model_input = whitespace["model_input"]
        .as_str()
        .expect("portable model input");
    let expected_model = whitespace["model_value"]
        .as_str()
        .expect("portable model value");

    assert_eq!(
        SubAgentConfig::new(model_input, "Research").model,
        expected_model
    );
    let restored: SubAgentConfig =
        serde_json::from_value(json!({"model": model_input})).expect("portable model wire value");
    assert_eq!(restored.model, expected_model);

    let blank_model = SubAgentConfig::new(
        whitespace["blank_model_input"]
            .as_str()
            .expect("portable blank model"),
        "Research",
    )
    .validate()
    .expect_err("portable blank model must fail");
    assert_eq!(
        blank_model.code(),
        fixture["validation"]["empty_model_error_code"]
    );

    let mut blank_prompt = SubAgentConfig::new("child-model", "Research");
    blank_prompt.system_prompt = Some(
        whitespace["blank_system_prompt_input"]
            .as_str()
            .expect("portable blank system prompt")
            .to_string(),
    );
    let blank_prompt = blank_prompt
        .validate()
        .expect_err("portable blank system prompt must fail");
    assert_eq!(
        blank_prompt.code(),
        fixture["validation"]["empty_system_prompt_error_code"]
    );

    assert_eq!(
        whitespace["edge_codepoints"],
        json!(["001C", "001D", "001E", "001F"])
    );
}

#[test]
fn sub_agent_config_from_wire_normalizes_model() {
    let fixture = super::contract();
    let restored: SubAgentConfig = serde_json::from_value(json!({
        "model": fixture["validation"]["normalized_model_input"],
    }))
    .expect("deserialize configured sub-agent");

    assert_eq!(
        restored.model,
        fixture["validation"]["normalized_model_value"]
    );
    assert_eq!(
        serde_json::json!({
            "description": restored.description,
            "backend": restored.backend,
            "system_prompt": restored.system_prompt,
            "max_cycles": restored.max_cycles,
            "exclude_tools": restored.exclude_tools,
            "denied_side_effects": restored.denied_side_effects,
            "denied_capability_tags": restored.denied_capability_tags,
            "deny_terminal_tools": restored.deny_terminal_tools,
            "denied_cost_dimensions": restored.denied_cost_dimensions,
            "metadata": restored.metadata,
        }),
        fixture["validation"]["wire_defaults"]
    );
}

#[test]
fn sub_agent_config_from_wire_rejects_invalid_values_immediately() {
    let fixture = super::contract();
    let empty_model = serde_json::from_value::<SubAgentConfig>(json!({
        "model": "  ",
    }))
    .expect_err("empty model must fail at the wire boundary");
    assert!(empty_model.to_string().contains(
        fixture["validation"]["empty_model_message"]
            .as_str()
            .unwrap()
    ));

    let empty_prompt = serde_json::from_value::<SubAgentConfig>(json!({
        "model": "child-model",
        "system_prompt": " \n ",
    }))
    .expect_err("empty system prompt must fail at the wire boundary");
    assert!(empty_prompt.to_string().contains(
        fixture["validation"]["empty_system_prompt_message"]
            .as_str()
            .unwrap()
    ));
}

#[test]
fn sub_agent_config_from_wire_rejects_shared_type_and_range_corpus() {
    let fixture = super::contract();
    let corpus = fixture["validation"]["wire_rejections"]
        .as_object()
        .expect("wire rejection corpus");
    let cases = [
        ("backend_non_string", "backend"),
        ("max_cycles_negative", "max_cycles"),
        ("denied_side_effects_non_array", "denied_side_effects"),
        ("denied_capability_tags_non_array", "denied_capability_tags"),
        ("deny_terminal_tools_non_boolean", "deny_terminal_tools"),
        ("denied_cost_dimensions_non_array", "denied_cost_dimensions"),
    ];
    assert_eq!(corpus.len(), cases.len());

    for (fixture_key, field) in cases {
        let mut payload = Map::from_iter([(
            "model".to_string(),
            Value::String("child-model".to_string()),
        )]);
        payload.insert(
            field.to_string(),
            corpus
                .get(fixture_key)
                .unwrap_or_else(|| panic!("missing wire rejection {fixture_key}"))
                .clone(),
        );

        assert!(
            serde_json::from_value::<SubAgentConfig>(Value::Object(payload)).is_err(),
            "wire rejection {fixture_key} must fail"
        );
    }
}

#[test]
fn configured_sub_agent_wire_rejects_unknown_top_level_fields() {
    let fixture = super::contract();
    assert_eq!(fixture["validation"]["unknown_top_level_fields"], "reject");
    assert!(serde_json::from_value::<SubAgentConfig>(json!({
        "model": "child-model",
        "backned": "invalid",
    }))
    .is_err());
    assert!(serde_json::from_value::<AgentTask>(json!({
        "task_id": "task",
        "model": "model",
        "system_prompt": "system",
        "user_prompt": "user",
        "runtime_metadata": {"trace_id": "invalid"},
    }))
    .is_err());
}
