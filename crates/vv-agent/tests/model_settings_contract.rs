use std::time::Duration;

use serde_json::json;
use vv_agent::{ModelSettings, ResponseFormat, RetrySettings, ToolChoice};

#[test]
fn model_settings_compact_wire_matches_the_python_contract() {
    let settings = ModelSettings::builder()
        .temperature(0.25)
        .top_p(0.8)
        .max_tokens(512)
        .tool_choice(ToolChoice::Auto)
        .parallel_tool_calls(false)
        .reasoning(json!({"effort": "high"}))
        .response_format(ResponseFormat::JsonObject)
        .timeout(Duration::from_secs_f64(12.5))
        .retry(RetrySettings::new(4).with_backoff_seconds(0.25))
        .extra_body("provider_option", json!(true))
        .extra_arg("request_option", json!("value"))
        .build();

    let payload = serde_json::to_value(&settings).expect("serialize settings");

    assert_eq!(
        payload,
        json!({
            "temperature": 0.25,
            "top_p": 0.8,
            "max_tokens": 512,
            "tool_choice": "auto",
            "parallel_tool_calls": false,
            "reasoning": {"effort": "high"},
            "response_format": {"type": "json_object"},
            "timeout_seconds": 12.5,
            "retry": {"max_attempts": 4, "backoff_seconds": 0.25},
            "extra_body": {"provider_option": true},
            "extra_args": {"request_option": "value"},
        })
    );
    assert_eq!(
        serde_json::from_value::<ModelSettings>(payload).expect("round trip"),
        settings
    );
}

#[test]
fn model_settings_reject_unknown_fields_and_invalid_ranges() {
    for payload in [
        json!({"unknown": true}),
        json!({"max_output_tokens": 256}),
        json!({"retry": {"unknown": true}}),
        json!({"max_tokens": 0}),
        json!({"timeout_seconds": 0}),
        json!({"top_p": 1.1}),
    ] {
        assert!(serde_json::from_value::<ModelSettings>(payload).is_err());
    }
}

#[test]
fn empty_retry_and_reasoning_use_canonical_defaults() {
    let settings: ModelSettings = serde_json::from_value(json!({
        "retry": {},
        "reasoning": {},
    }))
    .expect("settings");

    assert_eq!(settings.retry, Some(RetrySettings::default()));
    assert_eq!(settings.reasoning, None);
    assert_eq!(
        serde_json::to_value(settings).expect("serialize settings"),
        json!({"retry": {"max_attempts": 3, "backoff_seconds": 2.0}})
    );
}

#[test]
fn tool_choice_uses_modes_or_standard_named_tool_wire() {
    let named = json!({"type": "function", "function": {"name": "lookup"}});

    assert_eq!(
        serde_json::to_value(ToolChoice::Tool("lookup".to_string())).expect("serialize named tool"),
        named
    );
    assert_eq!(
        serde_json::from_value::<ToolChoice>(named).expect("named tool"),
        ToolChoice::Tool("lookup".to_string())
    );
    for invalid in [
        json!("lookup"),
        json!({"tool": "lookup"}),
        json!({"type": "function", "function": {"name": ""}}),
    ] {
        assert!(serde_json::from_value::<ToolChoice>(invalid).is_err());
    }

    assert!(ModelSettings::builder()
        .tool_choice(ToolChoice::Tool(String::new()))
        .build()
        .validate()
        .is_err());
}

#[test]
fn response_format_uses_closed_standard_wire() {
    let json_schema = json!({"name": "answer", "schema": {"type": "object"}, "strict": true});
    let json_schema_object = json_schema.as_object().expect("schema object").clone();
    let format = ResponseFormat::JsonSchema {
        json_schema: json_schema_object,
    };

    assert_eq!(
        serde_json::to_value(&format).expect("serialize response format"),
        json!({"type": "json_schema", "json_schema": json_schema})
    );
    assert_eq!(
        serde_json::from_value::<ResponseFormat>(json!({
            "type": "json_schema",
            "json_schema": {"name": "answer", "schema": {"type": "object"}, "strict": true}
        }))
        .expect("deserialize response format"),
        format
    );

    for invalid in [
        json!({}),
        json!({"type": "json_schema", "schema": json_schema}),
        json!({"type": "json_schema", "json_schema": []}),
        json!({"type": "json_object", "extra": true}),
    ] {
        assert!(
            serde_json::from_value::<ResponseFormat>(invalid.clone()).is_err(),
            "accepted invalid response format: {invalid}"
        );
    }
}
