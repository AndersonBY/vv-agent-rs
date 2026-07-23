use super::*;

#[test]
fn runtime_does_not_load_session_memory_by_default() {
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
    task.metadata
        .insert("enable_session_memory".to_string(), json!(true));
    task.metadata.insert(
        "session_memory_seed".to_string(),
        json!([{
            "category": "key_fact",
            "content": "seed must not enable session memory",
            "importance": 10
        }]),
    );

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    let first_request = inspector.first_request_messages();
    assert!(first_request.iter().all(|message| {
        !message.content.contains("<Session Memory>")
            && !message.content.contains("default session memory is loaded")
    }));
}

#[test]
fn runtime_rejects_non_boolean_session_memory_control() {
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

    let error = runtime
        .run(task)
        .expect_err("non-boolean control must fail");

    assert_eq!(
        error.to_string(),
        "llm request failed: session_memory_enabled must be a boolean"
    );
    assert!(inspector.first_request_messages().is_empty());
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
    task.metadata
        .insert("session_memory_enabled".to_string(), json!(true));

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
fn runtime_does_not_reuse_main_client_for_metadata_memory_route_without_provider() {
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
    assert_eq!(inspector.extraction_model(), None);
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
fn runtime_uses_main_client_for_default_memory_extraction_route() {
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
            .iter()
            .any(|message| message.content.contains("<Session Memory>")
                && message
                    .content
                    .contains("default callback preserved this fact")),
        "default session-memory route did not reuse the main client: {second_request:#?}"
    );
    assert_eq!(
        result
            .token_usage
            .model_calls
            .iter()
            .map(|call| call.operation)
            .collect::<Vec<_>>(),
        [
            vv_agent::ModelCallOperation::SessionMemory,
            vv_agent::ModelCallOperation::AgentCycle,
            vv_agent::ModelCallOperation::SessionMemory,
            vv_agent::ModelCallOperation::MemoryCompaction,
            vv_agent::ModelCallOperation::AgentCycle,
        ]
    );
}

#[test]
fn runtime_accounts_session_memory_usage_and_cache_before_agent_cycle() {
    let workspace = tempfile::tempdir().expect("workspace");
    let llm = AccountedSessionMemoryLlmClient::new(false);
    let inspector = llm.clone();
    let mut runtime = AgentRuntime::new(llm);
    runtime.default_workspace = Some(workspace.path().to_path_buf());
    let mut task = AgentTask::new("accounted-session", "demo", "system", "remember this");
    task.max_cycles = 1;
    task.no_tool_policy = vv_agent::NoToolPolicy::Finish;
    task.memory_compact_threshold = 10_000;
    task.metadata
        .insert("session_memory_enabled".to_string(), json!(true));
    task.metadata
        .insert("session_memory_min_tokens".to_string(), json!(1));
    task.metadata
        .insert("session_memory_min_text_messages".to_string(), json!(1));

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(inspector.main_call_count(), 1);
    assert_eq!(result.token_usage.model_calls.len(), 2);
    assert_eq!(
        result
            .token_usage
            .model_calls
            .iter()
            .map(|call| call.operation)
            .collect::<Vec<_>>(),
        [
            vv_agent::ModelCallOperation::SessionMemory,
            vv_agent::ModelCallOperation::AgentCycle,
        ]
    );
    assert_eq!(result.token_usage.input_tokens, Some(50));
    assert_eq!(result.token_usage.output_tokens, Some(12));
    assert_eq!(result.token_usage.total_tokens, Some(62));
    assert_eq!(result.token_usage.cache_usage.read_input_tokens, Some(18));
    assert_eq!(
        result.token_usage.cache_usage.uncached_input_tokens,
        Some(32)
    );
}

#[test]
fn runtime_emits_content_free_diagnostic_for_invalid_session_memory_output() {
    let workspace = tempfile::tempdir().expect("workspace");
    let llm = AccountedSessionMemoryLlmClient::new(true);
    let (events, event_handler) = run_event_collector();
    let mut runtime = AgentRuntime::new(llm);
    runtime.default_workspace = Some(workspace.path().to_path_buf());
    runtime.event_handler = Some(event_handler);
    let mut task = AgentTask::new("invalid-session", "demo", "system", "remember this");
    task.max_cycles = 1;
    task.no_tool_policy = vv_agent::NoToolPolicy::Finish;
    task.memory_compact_threshold = 10_000;
    task.metadata
        .insert("session_memory_enabled".to_string(), json!(true));
    task.metadata
        .insert("session_memory_min_tokens".to_string(), json!(1));
    task.metadata
        .insert("session_memory_min_text_messages".to_string(), json!(1));

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    let events = events.lock().expect("events");
    let details = events
        .iter()
        .find_map(|event| diagnostic_details(event, "session_memory_output_invalid"))
        .expect("invalid output diagnostic");
    assert_eq!(details["reason"], "json_array_missing");
    assert_eq!(details["backend"], "direct");
    assert_eq!(details["model"], "demo");
    for forbidden in ["prompt", "output", "messages", "error_text"] {
        assert!(!details.contains_key(forbidden));
    }
}

#[test]
fn session_memory_budget_exhaustion_stops_before_agent_cycle() {
    let workspace = tempfile::tempdir().expect("workspace");
    let llm = AccountedSessionMemoryLlmClient::new(false);
    let inspector = llm.clone();
    let (events, event_handler) = run_event_collector();
    let mut runtime = AgentRuntime::new(llm);
    runtime.default_workspace = Some(workspace.path().to_path_buf());
    let mut task = AgentTask::new("budgeted-session", "demo", "system", "remember this");
    task.max_cycles = 1;
    task.no_tool_policy = vv_agent::NoToolPolicy::Finish;
    task.memory_compact_threshold = 10_000;
    task.metadata
        .insert("session_memory_enabled".to_string(), json!(true));
    task.metadata
        .insert("session_memory_min_tokens".to_string(), json!(1));
    task.metadata
        .insert("session_memory_min_text_messages".to_string(), json!(1));
    let limits = RunBudgetLimits::builder()
        .max_total_tokens(20)
        .build()
        .expect("limits");

    let result = runtime
        .run_with_controls(
            task,
            RuntimeRunControls {
                budget_limits: Some(limits),
                event_handler: Some(event_handler),
                ..RuntimeRunControls::default()
            },
        )
        .expect("run");

    assert_eq!(result.status, AgentStatus::Failed);
    assert_eq!(
        result.completion_reason,
        Some(vv_agent::CompletionReason::BudgetExhausted)
    );
    assert_eq!(inspector.main_call_count(), 0);
    assert_eq!(result.token_usage.model_calls.len(), 1);
    assert_eq!(
        result.token_usage.model_calls[0].operation,
        vv_agent::ModelCallOperation::SessionMemory
    );
    let event_names = events
        .lock()
        .expect("events")
        .iter()
        .map(observable_event_name)
        .collect::<Vec<_>>();
    let terminal_index = event_names
        .iter()
        .position(|name| name == "model_call_completed")
        .expect("model terminal event");
    assert_eq!(event_names[terminal_index + 1], "budget_exhausted");
}

#[derive(Clone)]
struct AccountedSessionMemoryLlmClient {
    invalid_extraction: bool,
    main_calls: Arc<Mutex<usize>>,
}

impl AccountedSessionMemoryLlmClient {
    fn new(invalid_extraction: bool) -> Self {
        Self {
            invalid_extraction,
            main_calls: Arc::new(Mutex::new(0)),
        }
    }

    fn main_call_count(&self) -> usize {
        *self.main_calls.lock().expect("main calls")
    }
}

impl LlmClient for AccountedSessionMemoryLlmClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        if request.tools.is_empty()
            && request.messages.len() == 1
            && request.messages[0]
                .content
                .contains("extract durable facts that should survive context compression")
        {
            let mut response = LLMResponse::new(if self.invalid_extraction {
                "not a JSON array"
            } else {
                r#"[{"category":"key_fact","content":"accounted fact","importance":8}]"#
            });
            response.raw.insert(
                "usage".to_string(),
                json!({
                    "prompt_tokens": 20,
                    "completion_tokens": 5,
                    "total_tokens": 25,
                    "prompt_tokens_details": {"cached_tokens": 8}
                }),
            );
            return Ok(response);
        }
        *self.main_calls.lock().expect("main calls") += 1;
        let mut response = LLMResponse::new("done");
        response.raw.insert(
            "usage".to_string(),
            json!({
                "prompt_tokens": 30,
                "completion_tokens": 7,
                "total_tokens": 37,
                "prompt_tokens_details": {"cached_tokens": 10}
            }),
        );
        Ok(response)
    }
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
        if request.tools.is_empty()
            && request.messages.len() == 1
            && request.messages[0]
                .content
                .contains("<Conversation History>")
        {
            return Ok(LLMResponse::new(
                json!({
                    "summary_version": "2.0",
                    "key_facts": ["default callback preserved this fact"]
                })
                .to_string(),
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
