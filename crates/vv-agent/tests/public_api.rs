use std::collections::BTreeMap;

use vv_agent::{
    build_default_registry, load_llm_settings_from_file, resolve_model_endpoint, AgentDefinition,
    AgentRuntime, AgentSDKClient, AgentSDKOptions, AgentStatus, AgentTask, ConfigError,
    EndpointConfig, EndpointOption, LLMResponse, Message, ResolvedModelConfig, ScriptedLlmClient,
    ToolCall, ToolExecutionResult, ToolRegistry,
};

#[test]
fn top_level_types_are_constructible() {
    let _status = AgentStatus::Pending;
    let _message = Message::user("hello");
    let _tool_call = ToolCall::new("call_1", "echo", BTreeMap::new());
    let _result = ToolExecutionResult::success("call_1", "ok");
    let _llm_response = LLMResponse::new("done");
    let _registry = build_default_registry();
    let _definition = AgentDefinition::default_for_model("mini");
    let _task = AgentTask::new("task_1", "mini", "system", "user");
    let _runtime = AgentRuntime::new(ScriptedLlmClient::new(vec![LLMResponse::new("done")]));
    let _options = AgentSDKOptions::default();
    let _client = AgentSDKClient::new(_options);
    let _config_error = ConfigError::MissingSettingsFile("missing".to_string());
    let _endpoint = EndpointConfig::new("ep", "key", "http://localhost");
    let _endpoint_option = EndpointOption::new(_endpoint.clone(), "mini");
    let _resolved = ResolvedModelConfig::new(
        "moonshot",
        "kimi-k2.5",
        "kimi-k2-thinking",
        "kimi-k2-thinking",
        vec![],
    );
    let _ = load_llm_settings_from_file("missing.toml");
    let _ = resolve_model_endpoint(&serde_json::json!({}), "moonshot", "mini");
    let _registry_ref: &ToolRegistry = &_registry;
}
