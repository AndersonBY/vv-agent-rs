use super::*;

#[test]
fn runtime_loads_session_memory_by_default() {
    let workspace = tempfile::tempdir().expect("workspace");
    let storage_dir = workspace
        .path()
        .join(".memory/session/session_memory_default_task");
    fs::create_dir_all(&storage_dir).expect("session memory dir");
    fs::write(
        storage_dir.join("session_memory.json"),
        json!({
            "entries": [{
                "category": "key_fact",
                "content": "default session memory is loaded",
                "source_cycle": 3,
                "importance": 9
            }],
            "last_extracted_message_index": -1,
            "tokens_at_last_extraction": 0,
            "initialized": true
        })
        .to_string(),
    )
    .expect("session memory file");
    let llm = DefaultSessionMemoryInspectingLlmClient::default();
    let inspector = llm.clone();
    let mut runtime = AgentRuntime::new(llm);
    runtime.default_workspace = Some(workspace.path().to_path_buf());
    runtime.workspace_backend = Arc::new(vv_agent::workspace::LocalWorkspaceBackend::new(
        workspace.path(),
    ));
    let mut task = AgentTask::new(
        "session_memory_default_task",
        "demo",
        "system",
        "inspect memory",
    );
    task.max_cycles = 1;
    task.no_tool_policy = vv_agent::NoToolPolicy::Finish;

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    let first_request = inspector.first_request_messages();
    assert!(
        first_request
            .first()
            .is_some_and(|message| message.content.contains("<Session Memory>")
                && message.content.contains("default session memory is loaded")),
        "runtime did not load session memory by default: {first_request:#?}"
    );
}

#[test]
fn runtime_disables_session_memory_with_integer_zero() {
    let workspace = tempfile::tempdir().expect("workspace");
    let storage_dir = workspace.path().join(".memory/session/int_disabled_task");
    fs::create_dir_all(&storage_dir).expect("session memory dir");
    fs::write(
        storage_dir.join("session_memory.json"),
        json!({
            "entries": [{
                "category": "key_fact",
                "content": "integer zero should disable session memory",
                "source_cycle": 1,
                "importance": 9
            }],
            "last_extracted_message_index": -1,
            "tokens_at_last_extraction": 0,
            "initialized": true
        })
        .to_string(),
    )
    .expect("session memory file");
    let llm = DefaultSessionMemoryInspectingLlmClient::default();
    let inspector = llm.clone();
    let mut runtime = AgentRuntime::new(llm);
    runtime.default_workspace = Some(workspace.path().to_path_buf());
    runtime.workspace_backend = Arc::new(vv_agent::workspace::LocalWorkspaceBackend::new(
        workspace.path(),
    ));
    let mut task = AgentTask::new("int_disabled_task", "demo", "system", "inspect memory");
    task.max_cycles = 1;
    task.no_tool_policy = vv_agent::NoToolPolicy::Finish;
    task.metadata
        .insert("session_memory_enabled".to_string(), json!(0));

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    let first_request = inspector.first_request_messages();
    assert!(
        first_request
            .iter()
            .all(|message| !message.content.contains("<Session Memory>")
                && !message
                    .content
                    .contains("integer zero should disable session memory")),
        "integer 0 should disable session memory: {first_request:#?}"
    );
}

#[test]
fn runtime_scopes_session_memory_by_session_id_metadata() {
    let workspace = tempfile::tempdir().expect("workspace");
    let storage_dir = workspace.path().join(".memory/session/session-scope");
    fs::create_dir_all(&storage_dir).expect("session memory dir");
    fs::write(
        storage_dir.join("session_memory.json"),
        json!({
            "entries": [{
                "category": "key_fact",
                "content": "session scoped memory is loaded",
                "source_cycle": 2,
                "importance": 8
            }],
            "last_extracted_message_index": -1,
            "tokens_at_last_extraction": 0,
            "initialized": true
        })
        .to_string(),
    )
    .expect("session memory file");

    let llm = DefaultSessionMemoryInspectingLlmClient::default();
    let inspector = llm.clone();
    let mut runtime = AgentRuntime::new(llm);
    runtime.default_workspace = Some(workspace.path().to_path_buf());
    runtime.workspace_backend = Arc::new(vv_agent::workspace::LocalWorkspaceBackend::new(
        workspace.path(),
    ));
    let mut task = AgentTask::new(
        "fresh_task_id_for_same_session",
        "demo",
        "system",
        "inspect scoped memory",
    );
    task.max_cycles = 1;
    task.no_tool_policy = vv_agent::NoToolPolicy::Finish;
    task.metadata
        .insert("session_id".to_string(), json!("session-scope"));

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    let first_request = inspector.first_request_messages();
    assert!(
        first_request
            .first()
            .is_some_and(|message| message.content.contains("<Session Memory>")
                && message.content.contains("session scoped memory is loaded")),
        "runtime did not use session_id as session-memory storage scope: {first_request:#?}"
    );
}

#[test]
fn runtime_uses_memory_summary_metadata_model_for_session_extraction() {
    let workspace = tempfile::tempdir().expect("workspace");
    let settings_file = workspace.path().join("local_settings.json");
    fs::write(
        &settings_file,
        json!({
            "VV_AGENT_MEMORY_SUMMARY_BACKEND": "settings-backend",
            "VV_AGENT_MEMORY_SUMMARY_MODEL": "settings-model"
        })
        .to_string(),
    )
    .expect("settings file");
    let llm = SummaryModelInspectingLlmClient::default();
    let inspector = llm.clone();
    let mut runtime = AgentRuntime::new(llm)
        .with_settings_file(settings_file)
        .with_default_backend("fallback-backend");
    runtime.default_workspace = Some(workspace.path().to_path_buf());
    runtime.workspace_backend = Arc::new(vv_agent::workspace::LocalWorkspaceBackend::new(
        workspace.path(),
    ));
    let mut task = AgentTask::new(
        "summary_model_priority_task",
        "task-model",
        "system",
        "inspect memory",
    );
    task.memory_compact_threshold = 1;
    task.no_tool_policy = vv_agent::NoToolPolicy::Finish;
    task.metadata
        .insert("memory_summary_model".to_string(), json!("metadata-model"));
    task.metadata.insert(
        "memory_summary_backend".to_string(),
        json!("metadata-backend"),
    );
    task.metadata
        .insert("session_memory_min_tokens".to_string(), json!(1));
    task.metadata
        .insert("session_memory_min_text_messages".to_string(), json!(1));

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(
        inspector.extraction_model(),
        Some("metadata-model".to_string())
    );
}

#[test]
fn runtime_uses_local_memory_summary_model_defaults() {
    let workspace = tempfile::tempdir().expect("workspace");
    let settings_file = workspace.path().join("local_settings.json");
    fs::write(
        &settings_file,
        r#"
DEFAULT_USER_MEMORY_SUMMARIZE_BACKEND = "settings-backend"
DEFAULT_USER_MEMORY_SUMMARIZE_MODEL = "settings-model"
"#,
    )
    .expect("settings file");
    let llm = SummaryModelInspectingLlmClient::default();
    let inspector = llm.clone();
    let mut runtime = AgentRuntime::new(llm)
        .with_settings_file(settings_file)
        .with_default_backend("fallback-backend");
    runtime.default_workspace = Some(workspace.path().to_path_buf());
    runtime.workspace_backend = Arc::new(vv_agent::workspace::LocalWorkspaceBackend::new(
        workspace.path(),
    ));
    let mut task = AgentTask::new(
        "summary_model_settings_task",
        "task-model",
        "system",
        "inspect memory",
    );
    task.memory_compact_threshold = 1;
    task.no_tool_policy = vv_agent::NoToolPolicy::Finish;
    task.metadata
        .insert("session_memory_min_tokens".to_string(), json!(1));
    task.metadata
        .insert("session_memory_min_text_messages".to_string(), json!(1));

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(
        inspector.extraction_model(),
        Some("settings-model".to_string())
    );
}

#[test]
fn runtime_uses_settings_model_token_limits_for_direct_runtime_memory() {
    let workspace = tempfile::tempdir().expect("workspace");
    let settings_file = workspace.path().join("llm_settings.json");
    fs::write(
        &settings_file,
        json!({
            "VERSION": "2",
            "endpoints": [{
                "id": "deepseek-primary",
                "api_key": "sk-test",
                "api_base": "https://api.deepseek.test"
            }],
            "backends": {
                "deepseek": {
                    "models": {
                        "deepseek-v4-pro": {
                            "id": "deepseek-v4-pro",
                            "endpoints": ["deepseek-primary"],
                            "context_length": 1000,
                            "max_output_tokens": 100
                        }
                    }
                }
            }
        })
        .to_string(),
    )
    .expect("settings");

    let llm = DefaultSessionMemoryInspectingLlmClient::default();
    let inspector = llm.clone();
    let runtime = AgentRuntime::new(llm)
        .with_settings_file(settings_file)
        .with_default_backend("deepseek");
    let mut task = AgentTask::new(
        "direct_runtime_limits",
        "deepseek-v4-pro",
        "system",
        "Please remember this direct runtime token-limit check.",
    );
    task.max_cycles = 1;
    task.no_tool_policy = vv_agent::NoToolPolicy::Finish;
    task.memory_compact_threshold = 0;
    task.memory_threshold_percentage = 1;
    task.metadata
        .insert("include_memory_warning".to_string(), json!(true));
    task.metadata
        .insert("autocompact_buffer_tokens".to_string(), json!(0));
    task.metadata
        .insert("session_memory_enabled".to_string(), json!(false));

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    let first_request = inspector.first_request_messages();
    assert!(
        first_request
            .iter()
            .any(|message| message.content.contains("当前记忆已使用容量超过 1%")),
        "direct runtime did not use settings-derived token limits for memory warning: {first_request:#?}"
    );
}
#[test]
fn runtime_extracts_session_memory_with_default_llm_callback() {
    let workspace = tempfile::tempdir().expect("workspace");
    let large_tool_payload = "tool output ".repeat(300);
    let llm = SessionMemoryExtractingLlmClient::new(large_tool_payload);
    let inspector = llm.clone();
    let mut runtime = AgentRuntime::new(llm);
    runtime.default_workspace = Some(workspace.path().to_path_buf());
    runtime.workspace_backend = Arc::new(vv_agent::workspace::LocalWorkspaceBackend::new(
        workspace.path(),
    ));
    let mut task = AgentTask::new(
        "session_memory_extract_task",
        "demo",
        "system",
        "inspect memory",
    );
    task.memory_compact_threshold = 20;
    task.metadata
        .insert("model_context_window".to_string(), json!(120));
    task.metadata
        .insert("reserved_output_tokens".to_string(), json!(10));
    task.metadata
        .insert("autocompact_buffer_tokens".to_string(), json!(0));
    task.metadata
        .insert("session_memory_enabled".to_string(), json!(true));
    task.metadata
        .insert("session_memory_min_tokens".to_string(), json!(1));
    task.metadata
        .insert("session_memory_min_text_messages".to_string(), json!(1));

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(inspector.extraction_prompt_count(), 2);
    let second_request = inspector.second_request_messages();
    assert!(
        second_request
            .first()
            .is_some_and(|message| message.content.contains("<Session Memory>")
                && message
                    .content
                    .contains("default callback preserved this fact")),
        "second request did not include extracted session memory: {second_request:#?}"
    );
}
#[derive(Clone)]
struct SessionMemoryExtractingLlmClient {
    responses_seen: Arc<Mutex<usize>>,
    extraction_prompt_count: Arc<Mutex<usize>>,
    large_tool_payload: String,
    second_request_messages: Arc<Mutex<Vec<Message>>>,
}

impl SessionMemoryExtractingLlmClient {
    fn new(large_tool_payload: String) -> Self {
        Self {
            responses_seen: Arc::new(Mutex::new(0)),
            extraction_prompt_count: Arc::new(Mutex::new(0)),
            large_tool_payload,
            second_request_messages: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn extraction_prompt_count(&self) -> usize {
        *self
            .extraction_prompt_count
            .lock()
            .expect("extraction count poisoned")
    }

    fn second_request_messages(&self) -> Vec<Message> {
        self.second_request_messages
            .lock()
            .expect("messages poisoned")
            .clone()
    }
}

impl LlmClient for SessionMemoryExtractingLlmClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        if request.tools.is_empty()
            && request.messages.len() == 1
            && request.messages[0]
                .content
                .contains("extract durable facts that should survive context compression")
        {
            *self
                .extraction_prompt_count
                .lock()
                .expect("extraction count poisoned") += 1;
            return Ok(LLMResponse::new(
                r#"[{"category":"key_fact","content":"default callback preserved this fact","importance":9}]"#,
            ));
        }

        let mut responses_seen = self
            .responses_seen
            .lock()
            .map_err(|_| LlmError::Request("counter poisoned".to_string()))?;
        *responses_seen += 1;
        if *responses_seen == 1 {
            return Ok(LLMResponse::with_tool_calls(
                "first cycle",
                vec![ToolCall::new(
                    "write_large",
                    "write_file",
                    BTreeMap::from([
                        ("path".to_string(), json!("large.txt")),
                        (
                            "content".to_string(),
                            json!(self.large_tool_payload.clone()),
                        ),
                    ]),
                )],
            ));
        }
        *self
            .second_request_messages
            .lock()
            .expect("messages poisoned") = request.messages.clone();
        Ok(LLMResponse::with_tool_calls(
            "finish",
            vec![ToolCall::new(
                "finish_after_compact",
                "task_finish",
                BTreeMap::from([("message".to_string(), json!("memory compacted"))]),
            )],
        ))
    }
}

#[derive(Clone, Default)]
struct DefaultSessionMemoryInspectingLlmClient {
    first_request_messages: Arc<Mutex<Vec<Message>>>,
}

impl DefaultSessionMemoryInspectingLlmClient {
    fn first_request_messages(&self) -> Vec<Message> {
        self.first_request_messages
            .lock()
            .expect("messages poisoned")
            .clone()
    }
}

impl LlmClient for DefaultSessionMemoryInspectingLlmClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        let mut first_request = self
            .first_request_messages
            .lock()
            .map_err(|_| LlmError::Request("messages poisoned".to_string()))?;
        if first_request.is_empty() {
            *first_request = request.messages;
        }
        Ok(LLMResponse::new("done"))
    }
}

#[derive(Clone, Default)]
struct SummaryModelInspectingLlmClient {
    extraction_model: Arc<Mutex<Option<String>>>,
}

impl SummaryModelInspectingLlmClient {
    fn extraction_model(&self) -> Option<String> {
        self.extraction_model
            .lock()
            .expect("model poisoned")
            .clone()
    }
}

impl LlmClient for SummaryModelInspectingLlmClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        if request.tools.is_empty()
            && request.messages.len() == 1
            && request.messages[0]
                .content
                .contains("extract durable facts that should survive context compression")
        {
            *self
                .extraction_model
                .lock()
                .map_err(|_| LlmError::Request("model poisoned".to_string()))? =
                Some(request.model.clone());
            return Ok(LLMResponse::new(
                r#"[{"category":"key_fact","content":"summary model captured","importance":8}]"#,
            ));
        }
        Ok(LLMResponse::new("done"))
    }
}
