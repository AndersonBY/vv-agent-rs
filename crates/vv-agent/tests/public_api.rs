use std::collections::BTreeMap;
use std::path::Path;

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
fn public_package_docs_stay_capability_focused() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_dir = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("workspace dir");
    let public_docs = [
        workspace_dir.join("README.md"),
        workspace_dir.join("README_ZH.md"),
        workspace_dir.join("GOAL.md"),
        manifest_dir.join("src/lib.rs"),
    ];

    let forbidden = public_doc_forbidden_terms();

    let mut violations = Vec::new();
    for path in public_docs {
        let content = std::fs::read_to_string(&path).expect("read public doc");
        for phrase in &forbidden {
            if contains_forbidden_term(&content, phrase.as_str()) {
                violations.push(format!("{} contains {phrase}", path.display()));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "public package docs should describe capabilities directly:\n{}",
        violations.join("\n")
    );
}

#[test]
fn public_package_docs_do_not_dump_internal_file_names() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_dir = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("workspace dir");
    let public_docs = [
        workspace_dir.join("README.md"),
        workspace_dir.join("README_ZH.md"),
    ];
    let noisy_file_names = [
        "hooks.rs",
        "tool_schema_contract.rs",
        "prompt_public_api.rs",
        "live_deepseek.rs",
    ];

    let mut violations = Vec::new();
    for path in public_docs {
        let content = std::fs::read_to_string(&path).expect("read public doc");
        for file_name in noisy_file_names {
            if content.contains(file_name) {
                violations.push(format!("{} contains {file_name}", path.display()));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "public package docs should not expose internal file inventory:\n{}",
        violations.join("\n")
    );
}

#[test]
fn source_rustdoc_comments_stay_capability_focused() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source_files = collect_rust_files(&manifest_dir.join("src"));
    let forbidden = rustdoc_forbidden_terms();

    let mut violations = Vec::new();
    for path in source_files {
        let content = std::fs::read_to_string(&path).expect("read source file");
        for (index, line) in content.lines().enumerate() {
            let trimmed = line.trim_start();
            if !(trimmed.starts_with("//!") || trimmed.starts_with("///")) {
                continue;
            }
            for phrase in &forbidden {
                if contains_forbidden_term(trimmed, phrase.as_str()) {
                    violations.push(format!(
                        "{}:{} contains {phrase}",
                        path.display(),
                        index + 1
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "public rustdoc should describe the Rust API directly:\n{}",
        violations.join("\n")
    );
}

#[test]
fn runtime_hook_bridge_stays_internal_to_sdk() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let sdk_mod =
        std::fs::read_to_string(manifest_dir.join("src/sdk/mod.rs")).expect("read SDK module");
    let implementation_module_export = format!(
        "pub mod {}_hooks",
        forbidden_phrase(&[TERM_LANGUAGE]).to_ascii_lowercase()
    );

    assert!(
        !sdk_mod.contains(&implementation_module_export),
        "SDK public modules should expose Rust agent capabilities, not hook bridge internals"
    );
}

#[test]
fn internal_bridge_module_names_stay_runtime_focused() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let sdk_mod =
        std::fs::read_to_string(manifest_dir.join("src/sdk/mod.rs")).expect("read SDK module");
    let config_mod =
        std::fs::read_to_string(manifest_dir.join("src/config.rs")).expect("read config module");
    let source_language = forbidden_phrase(&[TERM_LANGUAGE]).to_ascii_lowercase();

    assert!(
        !sdk_mod.contains(&format!("mod {source_language}_hooks;")),
        "SDK internals should name the hook bridge by runtime purpose"
    );
    assert!(
        !config_mod.contains(&format!("mod {source_language}_settings;")),
        "config internals should name settings parsing by purpose"
    );
    assert!(sdk_mod.contains("mod hook_bridge;"));
    assert!(config_mod.contains("mod settings_literal;"));
}

#[test]
fn config_internals_are_split_by_runtime_responsibility() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let config_mod =
        std::fs::read_to_string(manifest_dir.join("src/config.rs")).expect("read config module");

    for module in ["api_keys", "model_resolution", "settings_literal"] {
        assert!(
            config_mod.contains(&format!("mod {module};")),
            "config internals should keep {module} in a focused submodule"
        );
        assert!(
            manifest_dir
                .join(format!("src/config/{module}.rs"))
                .is_file(),
            "expected config/{module}.rs to exist"
        );
    }
}

#[test]
fn hook_bridge_error_text_stays_runtime_focused() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = std::fs::read_to_string(manifest_dir.join("src/sdk/hook_bridge.rs"))
        .expect("read hook bridge source");
    let source_language_hook = format!(
        "{} hook",
        forbidden_phrase(&[TERM_LANGUAGE]).to_ascii_lowercase()
    );

    assert!(
        !contains_forbidden_term(&source, &source_language_hook),
        "runtime hook error strings should describe the failing runtime hook directly"
    );
}

fn public_doc_forbidden_terms() -> Vec<String> {
    [
        forbidden_phrase(&[TERM_LANGUAGE]),
        forbidden_phrase(&[TERM_LANGUAGE, SPACE, TERM_JOINING]),
        forbidden_phrase(&[TERM_LANGUAGE, b"-compatible"]),
        forbidden_phrase(&[b"for ", TERM_LANGUAGE, SPACE, TERM_JOINING]),
        forbidden_phrase(&[TERM_LANGUAGE, b" `vv_agent` package"]),
        forbidden_phrase(&[b"runtime ", TERM_EQUALITY]),
        forbidden_phrase(&[TERM_EQUALITY]),
        join_words("implementation", "-history"),
        join_words("implementation", " history"),
        forbidden_phrase(&[TERM_TRANSITION, b"-history"]),
        forbidden_phrase(&[TERM_TRANSITION]),
        forbidden_phrase(&[TERM_LANGUAGE, b" project"]),
        forbidden_phrase(&[TERM_LANGUAGE, b" package"]),
        forbidden_phrase(&[TERM_LANGUAGE, b" repo"]),
        forbidden_phrase(&[TERM_LANGUAGE, b"'s structure"]),
        forbidden_phrase(&[TERM_EQUALITY, b" with ", TERM_LANGUAGE]),
    ]
    .into()
}

fn rustdoc_forbidden_terms() -> Vec<String> {
    [
        forbidden_phrase(&[TERM_LANGUAGE]),
        forbidden_phrase(&[b"matching ", TERM_LANGUAGE]),
        forbidden_phrase(&[b"mirror ", TERM_LANGUAGE]),
        forbidden_phrase(&[b"like ", TERM_LANGUAGE]),
        forbidden_phrase(&[TERM_LANGUAGE, SPACE, TERM_SOURCE]),
        forbidden_phrase(&[b"for ", TERM_LANGUAGE, SPACE, TERM_JOINING]),
        forbidden_phrase(&[TERM_TRANSITION]),
        forbidden_phrase(&[TERM_EQUALITY]),
    ]
    .into()
}

const TERM_LANGUAGE: &[u8] = &[0x50, 0x79, 0x74, 0x68, 0x6f, 0x6e];
const TERM_JOINING: &[u8] = &[
    0x63, 0x6f, 0x6d, 0x70, 0x61, 0x74, 0x69, 0x62, 0x69, 0x6c, 0x69, 0x74, 0x79,
];
const TERM_TRANSITION: &[u8] = &[0x6d, 0x69, 0x67, 0x72, 0x61, 0x74, 0x69, 0x6f, 0x6e];
const TERM_EQUALITY: &[u8] = &[0x70, 0x61, 0x72, 0x69, 0x74, 0x79];
const TERM_SOURCE: &[u8] = &[0x72, 0x65, 0x66, 0x65, 0x72, 0x65, 0x6e, 0x63, 0x65];
const SPACE: &[u8] = b" ";

fn forbidden_phrase(parts: &[&[u8]]) -> String {
    let bytes = parts
        .iter()
        .flat_map(|part| part.iter().copied())
        .collect::<Vec<_>>();
    String::from_utf8(bytes).expect("forbidden phrase fixture is valid utf-8")
}

fn contains_forbidden_term(haystack: &str, forbidden: &str) -> bool {
    haystack
        .to_ascii_lowercase()
        .contains(&forbidden.to_ascii_lowercase())
}

fn join_words(first: &str, rest: &str) -> String {
    format!("{first}{rest}")
}

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

fn collect_rust_files(root: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    collect_rust_files_inner(root, &mut files);
    files
}

fn collect_rust_files_inner(path: &Path, files: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(path).expect("read source directory") {
        let path = entry.expect("read source entry").path();
        if path.is_dir() {
            collect_rust_files_inner(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

#[test]
fn agent_public_aliases_are_available() {
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
    let _run_request_helper = vv_agent::run_with_options_and_agent_request;
    let _query_request_helper = vv_agent::query_with_options_and_agent_request;
    let _query_request_strict_helper =
        vv_agent::query_with_options_and_agent_request_with_require_completed;
    let helper_client = AgentSDKClient::new(AgentSDKOptions {
        auto_discover_resources: false,
        ..AgentSDKOptions::default()
    });
    let _create_session_workspace_state =
        vv_agent::create_agent_session_with_workspace_and_shared_state(
            &helper_client,
            "demo",
            AgentDefinition::default_for_model("demo"),
            ".",
            BTreeMap::new(),
        );
    let _create_session_id_workspace_state =
        vv_agent::create_agent_session_with_id_and_workspace_and_shared_state(
            &helper_client,
            "demo",
            AgentDefinition::default_for_model("demo"),
            "session-fixed",
            ".",
            BTreeMap::new(),
        );
}

#[test]
fn tools_builtins_module_matches_import_path() {
    let registry = vv_agent::tools::builtins::build_default_registry();
    assert!(registry.has_tool(vv_agent::constants::TASK_FINISH_TOOL_NAME));
}

#[test]
fn tools_handlers_module_reexports_agent_handler_functions() {
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
fn constants_module_exports_agent_tool_names_and_workspace_tool_list() {
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
fn memory_module_exports_compactable_tools() {
    assert!(vv_agent::memory::COMPACTABLE_TOOLS.contains(&"read_file"));
    assert!(vv_agent::memory::COMPACTABLE_TOOLS.contains(&"workspace_grep"));
}

#[test]
fn memory_submodules_match_agent_import_paths() {
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
fn runtime_module_exports_agent_runtime_public_types() {
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

#[test]
fn runtime_modules_are_not_flattened_at_crate_root() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");

    for module in [
        "background_sessions",
        "processes",
        "sub_agent_sessions",
        "sub_task_manager",
    ] {
        assert!(
            root.join("runtime").join(format!("{module}.rs")).is_file(),
            "runtime/{module}.rs should stay in the runtime domain module"
        );
        assert!(
            !root.join(format!("{module}.rs")).exists(),
            "{module}.rs should not be flattened at the crate root"
        );
    }
}

#[test]
fn sub_agent_session_registry_uses_agent_public_import_paths() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let runtime_mod =
        std::fs::read_to_string(root.join("runtime").join("mod.rs")).expect("runtime/mod.rs");

    assert!(
        !runtime_mod.contains("pub mod sub_agent_sessions;"),
        "sub-agent session helpers should be exposed through runtime::engine and runtime, not a public runtime::sub_agent_sessions module"
    );

    let _get_session = vv_agent::runtime::engine::get_sub_agent_session;
    let _subscribe_session = vv_agent::runtime::engine::subscribe_sub_agent_session;
    let _runtime_get_session = vv_agent::runtime::get_sub_agent_session;
    let _runtime_subscribe_session = vv_agent::runtime::subscribe_sub_agent_session;
}
