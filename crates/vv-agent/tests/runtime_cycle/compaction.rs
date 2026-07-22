use super::*;

#[test]
fn runtime_compacts_memory_before_large_follow_up_cycle() {
    let workspace = tempfile::tempdir().expect("workspace");
    let large_tool_payload = "tool output ".repeat(300);
    let llm = MemoryCompactionInspectingLlmClient::new(large_tool_payload);
    let inspector = llm.clone();
    let mut runtime = AgentRuntime::new(llm);
    runtime.default_workspace = Some(workspace.path().to_path_buf());
    runtime.workspace_backend = Arc::new(vv_agent::workspace::LocalWorkspaceBackend::new(
        workspace.path(),
    ));
    let mut task = AgentTask::new("memory_task", "demo", "system", "inspect memory");
    task.memory_compact_threshold = 20;
    task.metadata
        .insert("model_context_window".to_string(), json!(120));
    task.metadata
        .insert("reserved_output_tokens".to_string(), json!(10));
    task.metadata
        .insert("autocompact_buffer_tokens".to_string(), json!(0));

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert!(result.cycles.iter().any(|cycle| cycle.memory_compacted));
    let second_request = inspector.second_request_messages();
    assert!(
        second_request
            .iter()
            .any(|message| message.content.contains("<Compressed Agent Memory>")),
        "second request did not contain compressed memory: {second_request:#?}"
    );
    assert!(
        second_request.len() <= 3,
        "compacted request should keep system, summary, and latest continuation at most"
    );
}

#[test]
fn runtime_uses_previous_prompt_tokens_for_memory_compaction() {
    let llm = PromptTokenCompactionInspectingLlmClient::default();
    let inspector = llm.clone();
    let runtime = AgentRuntime::new(llm);
    let mut task = AgentTask::new("usage_memory_task", "demo", "system", "short request");
    task.max_cycles = 2;
    task.metadata
        .insert("model_context_window".to_string(), json!(120));
    task.metadata
        .insert("reserved_output_tokens".to_string(), json!(10));
    task.metadata
        .insert("autocompact_buffer_tokens".to_string(), json!(10));

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    let second_request = inspector.second_request_messages();
    assert!(
        second_request
            .iter()
            .any(|message| message.content.contains("<Compressed Agent Memory>")),
        "runtime ignored previous provider prompt_tokens when preparing the next cycle: {second_request:#?}"
    );
}

#[test]
fn runtime_hooks_can_patch_messages_before_memory_compaction() {
    let workspace = tempfile::tempdir().expect("workspace");
    let large_tool_payload = "tool output ".repeat(300);
    let llm = MemoryCompactionInspectingLlmClient::new(large_tool_payload);
    let inspector = llm.clone();
    let hook = Arc::new(BeforeMemoryCompactHook::default());
    let mut runtime = AgentRuntime::new(llm);
    runtime.default_workspace = Some(workspace.path().to_path_buf());
    runtime.workspace_backend = Arc::new(vv_agent::workspace::LocalWorkspaceBackend::new(
        workspace.path(),
    ));
    runtime.hooks.push(hook.clone());
    let mut task = AgentTask::new("pre_compact_hook_task", "demo", "system", "inspect memory");
    task.memory_compact_threshold = 20;
    task.metadata
        .insert("model_context_window".to_string(), json!(120));
    task.metadata
        .insert("reserved_output_tokens".to_string(), json!(10));
    task.metadata
        .insert("autocompact_buffer_tokens".to_string(), json!(0));

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(hook.cycle_indexes(), vec![1, 2]);
    let second_request = inspector.second_request_messages();
    assert!(
        second_request
            .iter()
            .any(|message| message.content.contains("hook-added-before-memory-compact")),
        "second request did not include before-memory-compaction hook marker: {second_request:#?}"
    );
}

#[test]
fn runtime_injects_session_memory_context_after_compaction() {
    let workspace = tempfile::tempdir().expect("workspace");
    let large_tool_payload = "tool output ".repeat(300);
    let llm = MemoryCompactionInspectingLlmClient::new(large_tool_payload);
    let inspector = llm.clone();
    let mut runtime = AgentRuntime::new(llm);
    runtime.default_workspace = Some(workspace.path().to_path_buf());
    runtime.workspace_backend = Arc::new(vv_agent::workspace::LocalWorkspaceBackend::new(
        workspace.path(),
    ));
    let mut task = AgentTask::new("session_memory_task", "demo", "system", "inspect memory");
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
    task.metadata.insert(
        "session_memory_seed".to_string(),
        json!([
            {"category":"key_fact","content":"sub-agent findings survive compaction","importance":9,"source_cycle":0}
        ]),
    );

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    let second_request = inspector.second_request_messages();
    assert!(
        second_request
            .first()
            .is_some_and(|message| message.content.contains("<Session Memory>")
                && message
                    .content
                    .contains("sub-agent findings survive compaction")),
        "second request did not include session memory context: {second_request:#?}"
    );
}
#[derive(Default)]
struct BeforeMemoryCompactHook {
    cycle_indexes: Mutex<Vec<u32>>,
}

impl BeforeMemoryCompactHook {
    fn cycle_indexes(&self) -> Vec<u32> {
        self.cycle_indexes
            .lock()
            .expect("cycle indexes poisoned")
            .clone()
    }
}

impl RuntimeHook for BeforeMemoryCompactHook {
    fn before_memory_compact(
        &self,
        event: vv_agent::BeforeMemoryCompactEvent<'_>,
    ) -> Option<Vec<Message>> {
        self.cycle_indexes
            .lock()
            .expect("cycle indexes poisoned")
            .push(event.cycle_index);
        assert_eq!(event.task.task_id, "pre_compact_hook_task");
        assert!(event.shared_state.contains_key("todo_list"));
        if event.cycle_index != 2 {
            return None;
        }
        assert!(event
            .messages
            .iter()
            .flat_map(|message| message.tool_calls.iter())
            .any(|call| call
                .arguments
                .get("content")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|content| content.contains("tool output"))));
        let mut messages = event.messages.to_vec();
        messages.push(Message::user("hook-added-before-memory-compact"));
        Some(messages)
    }
}
#[derive(Clone)]
struct MemoryCompactionInspectingLlmClient {
    responses_seen: Arc<Mutex<usize>>,
    large_tool_payload: String,
    second_request_messages: Arc<Mutex<Vec<Message>>>,
}

impl MemoryCompactionInspectingLlmClient {
    fn new(large_tool_payload: String) -> Self {
        Self {
            responses_seen: Arc::new(Mutex::new(0)),
            large_tool_payload,
            second_request_messages: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn second_request_messages(&self) -> Vec<Message> {
        self.second_request_messages
            .lock()
            .expect("messages poisoned")
            .clone()
    }
}

impl LlmClient for MemoryCompactionInspectingLlmClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
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
struct PromptTokenCompactionInspectingLlmClient {
    responses_seen: Arc<Mutex<usize>>,
    second_request_messages: Arc<Mutex<Vec<Message>>>,
}

impl PromptTokenCompactionInspectingLlmClient {
    fn second_request_messages(&self) -> Vec<Message> {
        self.second_request_messages
            .lock()
            .expect("messages poisoned")
            .clone()
    }
}

impl LlmClient for PromptTokenCompactionInspectingLlmClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        let mut responses_seen = self
            .responses_seen
            .lock()
            .map_err(|_| LlmError::Request("counter poisoned".to_string()))?;
        *responses_seen += 1;
        if *responses_seen == 1 {
            let mut response = LLMResponse::new("continue after measuring prompt tokens");
            response.token_usage = TokenUsage {
                input_tokens: Some(101),
                total_tokens: Some(120),
                ..TokenUsage::default()
            };
            return Ok(response);
        }
        *self
            .second_request_messages
            .lock()
            .expect("messages poisoned") = request.messages.clone();
        Ok(LLMResponse::with_tool_calls(
            "finish",
            vec![ToolCall::new(
                "finish_after_prompt_usage_compaction",
                "task_finish",
                BTreeMap::from([("message".to_string(), json!("memory compacted"))]),
            )],
        ))
    }
}
