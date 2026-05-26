use std::collections::BTreeMap;

use vv_agent::{
    background_session_manager, build_default_registry, load_llm_settings_from_file,
    resolve_model_endpoint, AgentDefinition, AgentRuntime, AgentSDKClient, AgentSDKOptions,
    AgentStatus, AgentTask, BackgroundSessionListener, CancellationToken, CeleryBackend,
    Checkpoint, ConfigError, EndpointConfig, EndpointOption, FileInfo, InMemoryStateStore,
    InlineBackend, LLMResponse, LocalWorkspaceBackend, MemoryWorkspaceBackend, Message,
    ResolvedModelConfig, RuntimeRecipe, RuntimeRunControls, S3WorkspaceBackend, S3WorkspaceConfig,
    ScriptedLlmClient, SessionCancellationHandle, SqliteStateStore, StateStore, ThreadBackend,
    ToolCall, ToolExecutionResult, ToolRegistry, WorkspaceBackend,
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
    let _token = CancellationToken::default();
    let _controls = RuntimeRunControls::default();
    let _inline_backend = InlineBackend;
    let _thread_backend = ThreadBackend::default();
    let _recipe = RuntimeRecipe::new("settings.py", "backend", "model", ".");
    let _celery_backend = CeleryBackend::inline_fallback();
    let _checkpoint = Checkpoint {
        task_id: "task".to_string(),
        cycle_index: 0,
        status: AgentStatus::Pending,
        messages: vec![],
        cycles: vec![],
        shared_state: BTreeMap::new(),
    };
    let _state_store = InMemoryStateStore::default();
    let _state_store_ref: &dyn StateStore = &_state_store;
    let _sqlite_state_store = SqliteStateStore::new(":memory:");
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
    let _file_info = FileInfo {
        path: "notes.md".to_string(),
        is_file: true,
        is_dir: false,
        size: 5,
        modified_at: "0".to_string(),
        suffix: ".md".to_string(),
    };
    let _local = LocalWorkspaceBackend::new(".");
    let _memory = MemoryWorkspaceBackend::default();
    let _s3_config = S3WorkspaceConfig::new("bucket");
    let _s3 = S3WorkspaceBackend::default();
    let _workspace: &dyn WorkspaceBackend = &_memory;
    let _listener: BackgroundSessionListener = std::sync::Arc::new(|_| {});
    let _session_cancellation: Option<SessionCancellationHandle> = None;
    let _ = background_session_manager();
}
