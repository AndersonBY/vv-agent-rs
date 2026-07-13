use serde_json::{json, Value};
use vv_agent::{
    Agent, EndpointConfig, EndpointOption, LLMResponse, ModelRef, ResolvedModelConfig, Runner,
    ScriptedModelProvider, ToolCall,
};

const FIXTURE: &str = include_str!("fixtures/parity/model_ref_v1.json");

#[test]
fn model_ref_wire_matches_the_shared_closed_contract() {
    let fixture: Value = serde_json::from_str(FIXTURE).expect("fixture");
    let valid = fixture["valid"].as_array().expect("valid cases");

    for payload in valid {
        let model: ModelRef = serde_json::from_value(payload.clone()).expect("valid ModelRef");
        assert_eq!(
            serde_json::to_value(model).expect("serialize ModelRef"),
            *payload
        );
    }
    for payload in fixture["invalid"].as_array().expect("invalid cases") {
        assert!(serde_json::from_value::<ModelRef>(payload.clone()).is_err());
    }
}

#[test]
fn resolved_model_ref_cannot_serialize_credentials() {
    let endpoint = EndpointConfig::new(
        "private",
        "must-not-serialize",
        "https://example.invalid/v1",
    );
    let resolved = ResolvedModelConfig::new(
        "private",
        "demo",
        "demo",
        "demo",
        vec![EndpointOption::new(endpoint, "demo")],
    );

    let error =
        serde_json::to_string(&ModelRef::resolved(resolved)).expect_err("resolved must fail");

    assert!(error.to_string().contains("process-local"));
    assert!(!error.to_string().contains("must-not-serialize"));
}

#[tokio::test]
async fn scripted_provider_default_model_is_runner_fallback() {
    let provider = ScriptedModelProvider::new(
        "scripted",
        "provider-default",
        vec![LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::from_raw_arguments(
                "finish",
                "task_finish",
                json!({"message": "done"}),
            )],
        )],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(".")
        .build()
        .expect("runner");
    let agent = Agent::builder("assistant")
        .instructions("Finish.")
        .build()
        .expect("agent");

    let result = runner.run(&agent, "go").await.expect("run");

    let resolved = result.resolved_model().expect("resolved model");
    assert_eq!(resolved.model_id, "provider-default");
    assert!(resolved.function_call_available);
    assert!(resolved.response_format_available);
}
