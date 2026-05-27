use std::collections::BTreeMap;

use vv_agent::{
    background_session_manager, build_default_registry, dispatch_tool_call,
    load_llm_settings_from_file, resolve_model_endpoint, AfterLLMEvent, AgentDefinition,
    AgentRuntime, AgentSDKClient, AgentSDKOptions, AgentStatus, AgentTask,
    BackgroundSessionListener, BaseRuntimeHook, BeforeLLMEvent, BeforeLLMPatch, CancellationToken,
    CeleryBackend, Checkpoint, ConfigError, EndpointConfig, EndpointOption, ExecutionBackend,
    ExecutionContext, FileInfo, InMemoryStateStore, InlineBackend, LLMClient, LLMResponse,
    LocalWorkspaceBackend, MemoryWorkspaceBackend, Message, RedisStateStore, ResolvedModelConfig,
    RuntimeExecutionBackend, RuntimeRecipe, RuntimeRunControls, S3WorkspaceBackend,
    S3WorkspaceConfig, ScriptedLLM, ScriptedLlmClient, SessionCancellationHandle, SqliteStateStore,
    StateStore, ThreadBackend, ToolCall, ToolExecutionResult, ToolNotFoundError, ToolRegistry,
    VVLlmClient, WorkspaceBackend,
};

#[test]
fn top_level_types_are_constructible() {
    let _status = AgentStatus::Pending;
    let _message = Message::user("hello");
    let _tool_call = ToolCall::new("call_1", "echo", BTreeMap::new());
    let _result = ToolExecutionResult::success("call_1", "ok");
    let _llm_response = LLMResponse::new("done");
    let _registry = build_default_registry();
    let _dispatch = dispatch_tool_call;
    let _tool_not_found = ToolNotFoundError("missing_tool".to_string());
    let _definition = AgentDefinition::default_for_model("mini");
    let _task = AgentTask::new("task_1", "mini", "system", "user");
    let _runtime = AgentRuntime::new(ScriptedLlmClient::new(vec![LLMResponse::new("done")]))
        .with_settings_file("settings.py")
        .with_default_backend("deepseek")
        .with_sub_agent_timeout_seconds(30.0);
    let _token = CancellationToken::default();
    let _context = ExecutionContext::default();
    let _controls = RuntimeRunControls::default();
    let _inline_backend = InlineBackend;
    let _execution_backend = RuntimeExecutionBackend::default();
    let _execution_backend_alias = ExecutionBackend::default();
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
    let _redis_key = RedisStateStore::checkpoint_key("task");
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

#[test]
fn python_style_public_aliases_are_available() {
    fn assert_llm_client<T: LLMClient>() {}
    assert_llm_client::<ScriptedLLM>();

    let _scripted = ScriptedLLM::new(vec![LLMResponse::new("done")]);
    let _vv_llm: Option<VVLlmClient> = None;
    let _backend = ExecutionBackend::default();
    let _before_patch = BeforeLLMPatch::default();
    let _before_event: Option<BeforeLLMEvent<'_>> = None;
    let _after_event: Option<AfterLLMEvent<'_>> = None;
    let _hook: Option<&dyn BaseRuntimeHook> = None;
    let _sdk_llm_builder: Option<vv_agent::sdk::LLMBuilder> = None;
    let _sdk_runtime_log_handler: Option<vv_agent::sdk::RuntimeLogHandler> = None;
}

#[test]
fn tools_builtins_module_matches_python_import_path() {
    let registry = vv_agent::tools::builtins::build_default_registry();
    assert!(registry.has_tool(vv_agent::constants::TASK_FINISH_TOOL_NAME));
}

#[test]
fn tools_handlers_module_reexports_python_handler_functions() {
    fn assert_handler(
        _: fn(
            &mut vv_agent::ToolContext,
            &vv_agent::types::ToolArguments,
        ) -> vv_agent::ToolExecutionResult,
    ) {
    }

    assert_handler(vv_agent::tools::handlers::activate_skill);
    assert_handler(vv_agent::tools::handlers::ask_user);
    assert_handler(vv_agent::tools::handlers::check_background_command);
    assert_handler(vv_agent::tools::handlers::compress_memory);
    assert_handler(vv_agent::tools::handlers::create_sub_task);
    assert_handler(vv_agent::tools::handlers::file_info);
    assert_handler(vv_agent::tools::handlers::file_str_replace);
    assert_handler(vv_agent::tools::handlers::list_files);
    assert_handler(vv_agent::tools::handlers::read_file);
    assert_handler(vv_agent::tools::handlers::read_image);
    assert_handler(vv_agent::tools::handlers::run_bash_command);
    assert_handler(vv_agent::tools::handlers::sub_task_status);
    assert_handler(vv_agent::tools::handlers::task_finish);
    assert_handler(vv_agent::tools::handlers::todo_read);
    assert_handler(vv_agent::tools::handlers::todo_write);
    assert_handler(vv_agent::tools::handlers::workspace_grep);
    assert_handler(vv_agent::tools::handlers::write_file);

    assert_handler(vv_agent::tools::handlers::background::check_background_command);
    assert_handler(vv_agent::tools::handlers::bash::run_bash_command);
    assert_handler(vv_agent::tools::handlers::control::ask_user);
    assert_handler(vv_agent::tools::handlers::control::task_finish);
    assert_handler(vv_agent::tools::handlers::image::read_image);
    assert_handler(vv_agent::tools::handlers::memory::compress_memory);
    assert_handler(vv_agent::tools::handlers::search::workspace_grep);
    assert_handler(vv_agent::tools::handlers::skills::activate_skill);
    assert_handler(vv_agent::tools::handlers::sub_agents::create_sub_task);
    assert_handler(vv_agent::tools::handlers::sub_task_status::sub_task_status);
    assert_handler(vv_agent::tools::handlers::workspace_io::file_info);
    assert_handler(vv_agent::tools::handlers::workspace_io::file_str_replace);
    assert_handler(vv_agent::tools::handlers::workspace_io::list_files);
    assert_handler(vv_agent::tools::handlers::workspace_io::read_file);
    assert_handler(vv_agent::tools::handlers::workspace_io::write_file);
}

#[test]
fn constants_module_exports_python_tool_names_and_workspace_tool_list() {
    use vv_agent::constants;

    assert_eq!(constants::TODO_INCOMPLETE_ERROR_CODE, "todo_incomplete");
    assert_eq!(constants::ASK_USER_TOOL_NAME, "ask_user");
    assert_eq!(constants::TASK_FINISH_TOOL_NAME, "task_finish");
    assert_eq!(constants::READ_FILE_TOOL_NAME, "read_file");
    assert_eq!(constants::WRITE_FILE_TOOL_NAME, "write_file");
    assert_eq!(constants::LIST_FILES_TOOL_NAME, "list_files");
    assert_eq!(constants::FILE_STR_REPLACE_TOOL_NAME, "file_str_replace");
    assert_eq!(constants::WORKSPACE_GREP_TOOL_NAME, "workspace_grep");
    assert_eq!(constants::BASH_TOOL_NAME, "bash");
    assert_eq!(
        constants::CHECK_BACKGROUND_COMMAND_TOOL_NAME,
        "check_background_command"
    );
    assert_eq!(constants::CREATE_SUB_TASK_TOOL_NAME, "create_sub_task");
    assert_eq!(constants::SUB_TASK_STATUS_TOOL_NAME, "sub_task_status");
    assert_eq!(constants::COMPRESS_MEMORY_TOOL_NAME, "compress_memory");
    assert_eq!(constants::TODO_WRITE_TOOL_NAME, "todo_write");
    assert_eq!(constants::READ_IMAGE_TOOL_NAME, "read_image");
    assert_eq!(constants::FILE_INFO_TOOL_NAME, "file_info");
    assert_eq!(constants::ACTIVATE_SKILL_TOOL_NAME, "activate_skill");
    assert_eq!(
        constants::WORKSPACE_TOOLS,
        [
            constants::LIST_FILES_TOOL_NAME,
            constants::FILE_INFO_TOOL_NAME,
            constants::READ_FILE_TOOL_NAME,
            constants::WRITE_FILE_TOOL_NAME,
            constants::FILE_STR_REPLACE_TOOL_NAME,
            constants::WORKSPACE_GREP_TOOL_NAME,
            constants::COMPRESS_MEMORY_TOOL_NAME,
            constants::TODO_WRITE_TOOL_NAME,
        ]
    );

    let default_schemas = constants::get_default_tool_schemas();
    assert!(default_schemas.contains_key(constants::TASK_FINISH_TOOL_NAME));
    assert!(default_schemas.contains_key(constants::ASK_USER_TOOL_NAME));
    assert!(default_schemas.contains_key(constants::ACTIVATE_SKILL_TOOL_NAME));

    let workspace_schemas = constants::workspace_tools_schemas();
    assert_eq!(workspace_schemas.len(), constants::WORKSPACE_TOOLS.len());
    assert!(workspace_schemas.contains_key(constants::READ_FILE_TOOL_NAME));
    assert!(!workspace_schemas.contains_key(constants::TASK_FINISH_TOOL_NAME));

    assert_eq!(
        constants::task_finish_tool_schema()["function"]["name"],
        constants::TASK_FINISH_TOOL_NAME
    );
    assert_eq!(
        constants::ask_user_tool_schema()["function"]["name"],
        constants::ASK_USER_TOOL_NAME
    );
    assert_eq!(
        constants::activate_skill_tool_schema()["function"]["name"],
        constants::ACTIVATE_SKILL_TOOL_NAME
    );
    assert_eq!(
        constants::tool_names::ASK_USER_TOOL_NAME,
        constants::ASK_USER_TOOL_NAME
    );
    assert_eq!(
        constants::workspace::WORKSPACE_TOOLS[0],
        constants::LIST_FILES_TOOL_NAME
    );
    assert!(constants::workspace::get_default_tool_schemas()
        .contains_key(constants::TASK_FINISH_TOOL_NAME));
    assert_eq!(
        constants::workspace::task_finish_tool_schema()["function"]["name"],
        constants::TASK_FINISH_TOOL_NAME
    );
    assert_eq!(
        constants::workspace::TASK_FINISH_TOOL_SCHEMA()["function"]["name"],
        constants::TASK_FINISH_TOOL_NAME
    );
    assert_eq!(
        constants::workspace::ASK_USER_TOOL_SCHEMA()["function"]["name"],
        constants::ASK_USER_TOOL_NAME
    );
    assert_eq!(
        constants::workspace::ACTIVATE_SKILL_TOOL_SCHEMA()["function"]["name"],
        constants::ACTIVATE_SKILL_TOOL_NAME
    );
    assert!(constants::workspace::WORKSPACE_TOOLS_SCHEMAS()
        .contains_key(constants::READ_FILE_TOOL_NAME));
    assert!(
        constants::TASK_FINISH_TOOL_SCHEMA()["function"]["name"]
            == constants::TASK_FINISH_TOOL_NAME
    );
}

#[test]
fn memory_module_exports_compactable_tools_like_python() {
    assert!(vv_agent::memory::COMPACTABLE_TOOLS.contains(&"read_file"));
    assert!(vv_agent::memory::COMPACTABLE_TOOLS.contains(&"workspace_grep"));
}

#[test]
fn memory_submodules_match_python_import_paths() {
    let _error =
        vv_agent::memory::errors::CompactionExhaustedError::new(2, Some("last".to_string()));
    let _manager_config = vv_agent::memory::manager::MemoryManagerConfig::default();
    let _microcompact_config = vv_agent::memory::microcompact::MicrocompactConfig::default();
    assert!(vv_agent::memory::microcompact::COMPACTABLE_TOOLS.contains(&"workspace_grep"));
    let _restore_config =
        vv_agent::memory::post_compact_restore::PostCompactRestoreConfig::default();
    let _session_config = vv_agent::memory::session_memory::SessionMemoryConfig::default();
    let _session_state = vv_agent::memory::session_memory::SessionMemoryState::default();
    let sanitized = vv_agent::memory::message_sanitizer::sanitize_for_resume(&[]);
    assert!(sanitized.is_empty());
}

#[test]
fn runtime_module_exports_python_runtime_public_types() {
    let _inline = vv_agent::runtime::InlineBackend;
    let _cancelled = vv_agent::runtime::CancelledError::new("Operation was cancelled");
    let _managed: Option<vv_agent::runtime::ManagedSubTask> = None;
    let manager = vv_agent::runtime::RuntimeHookManager::default();
    assert!(!manager.has_hooks());
    assert_eq!(vv_agent::runtime::cycle_runner::MAX_PTL_RETRIES, 3);
    assert_eq!(vv_agent::runtime::MAX_PTL_RETRIES, 3);
    assert_eq!(vv_agent::MAX_PTL_RETRIES, 3);
    let _checkpoint = vv_agent::runtime::Checkpoint {
        task_id: "task".to_string(),
        cycle_index: 0,
        status: vv_agent::AgentStatus::Pending,
        messages: vec![],
        cycles: vec![],
        shared_state: BTreeMap::new(),
    };
    let state_store = vv_agent::runtime::InMemoryStateStore::default();
    let _state_store_ref: &dyn vv_agent::runtime::StateStore = &state_store;
    let _get_session = vv_agent::runtime::engine::get_sub_agent_session;
    let _subscribe_session = vv_agent::runtime::engine::subscribe_sub_agent_session;
    let _steer_session = vv_agent::runtime::engine::steer_sub_agent_session;
}
