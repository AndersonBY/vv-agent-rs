use super::*;

#[test]
fn runtime_microcompacts_before_full_memory_compaction() {
    let workspace = tempfile::tempdir().expect("workspace");
    let large_tool_payload = "tool output ".repeat(300);
    let llm = MicrocompactInspectingLlmClient::new(large_tool_payload);
    let inspector = llm.clone();
    let mut runtime = AgentRuntime::new(llm);
    runtime.default_workspace = Some(workspace.path().to_path_buf());
    runtime.workspace_backend = Arc::new(vv_agent::workspace::LocalWorkspaceBackend::new(
        workspace.path(),
    ));
    let mut task = AgentTask::new("microcompact_task", "demo", "system", "inspect memory");
    task.extra_tool_names.push("bash".to_string());
    task.memory_compact_threshold = 10_000;
    task.metadata
        .insert("model_context_window".to_string(), json!(20_000));
    task.metadata
        .insert("reserved_output_tokens".to_string(), json!(0));
    task.metadata
        .insert("autocompact_buffer_tokens".to_string(), json!(0));
    task.metadata
        .insert("microcompact_trigger_ratio".to_string(), json!(0.01));
    task.metadata
        .insert("microcompact_keep_recent_cycles".to_string(), json!(0));
    task.metadata
        .insert("microcompact_min_result_length".to_string(), json!(200));

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    let second_request = inspector.third_request_messages();
    assert!(
        second_request
            .iter()
            .any(|message| message.content == CLEARED_MARKER),
        "second request did not include microcompacted tool content: {second_request:#?}"
    );
    assert!(
        second_request
            .iter()
            .all(|message| !message.content.contains("<Compressed Agent Memory>")),
        "microcompact should avoid full summary before threshold: {second_request:#?}"
    );
}

#[test]
fn runtime_respects_configured_microcompact_tool_allowlist() {
    let workspace = tempfile::tempdir().expect("workspace");
    let large_tool_payload = "tool output ".repeat(300);
    let llm = MicrocompactInspectingLlmClient::new(large_tool_payload);
    let inspector = llm.clone();
    let mut runtime = AgentRuntime::new(llm);
    runtime.default_workspace = Some(workspace.path().to_path_buf());
    runtime.workspace_backend = Arc::new(vv_agent::workspace::LocalWorkspaceBackend::new(
        workspace.path(),
    ));
    let mut task = AgentTask::new(
        "microcompact_allowlist_task",
        "demo",
        "system",
        "inspect memory",
    );
    task.extra_tool_names.push("bash".to_string());
    task.memory_compact_threshold = 10_000;
    task.metadata
        .insert("model_context_window".to_string(), json!(20_000));
    task.metadata
        .insert("reserved_output_tokens".to_string(), json!(0));
    task.metadata
        .insert("autocompact_buffer_tokens".to_string(), json!(0));
    task.metadata
        .insert("microcompact_trigger_ratio".to_string(), json!(0.01));
    task.metadata
        .insert("microcompact_keep_recent_cycles".to_string(), json!(0));
    task.metadata
        .insert("microcompact_min_result_length".to_string(), json!(200));
    task.metadata.insert(
        "microcompact_compactable_tools".to_string(),
        json!(["read_file"]),
    );

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    let third_request = inspector.third_request_messages();
    assert!(
        third_request
            .iter()
            .all(|message| message.content != CLEARED_MARKER),
        "bash output should not be microcompacted when only read_file is allowlisted: {third_request:#?}"
    );
    assert!(
        third_request
            .iter()
            .any(|message| message.content.contains("tool output")),
        "original bash output should remain available: {third_request:#?}"
    );
}

#[test]
fn runtime_parses_string_float_microcompact_ratio() {
    let workspace = tempfile::tempdir().expect("workspace");
    let large_tool_payload = "tool output ".repeat(300);
    let llm = MicrocompactInspectingLlmClient::new(large_tool_payload);
    let inspector = llm.clone();
    let mut runtime = AgentRuntime::new(llm);
    runtime.default_workspace = Some(workspace.path().to_path_buf());
    runtime.workspace_backend = Arc::new(vv_agent::workspace::LocalWorkspaceBackend::new(
        workspace.path(),
    ));
    let mut task = AgentTask::new(
        "microcompact_string_ratio_task",
        "demo",
        "system",
        "inspect memory",
    );
    task.extra_tool_names.push("bash".to_string());
    task.memory_compact_threshold = 10_000;
    task.metadata
        .insert("model_context_window".to_string(), json!(20_000));
    task.metadata
        .insert("reserved_output_tokens".to_string(), json!(0));
    task.metadata
        .insert("autocompact_buffer_tokens".to_string(), json!(0));
    task.metadata
        .insert("microcompact_trigger_ratio".to_string(), json!("0.01"));
    task.metadata
        .insert("microcompact_keep_recent_cycles".to_string(), json!(0));
    task.metadata
        .insert("microcompact_min_result_length".to_string(), json!(200));

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    let third_request = inspector.third_request_messages();
    assert!(
        third_request
            .iter()
            .any(|message| message.content == CLEARED_MARKER),
        "string float microcompact ratio should trigger the same compaction: {third_request:#?}"
    );
}
#[derive(Clone)]
struct MicrocompactInspectingLlmClient {
    responses_seen: Arc<Mutex<usize>>,
    large_tool_payload: String,
    third_request_messages: Arc<Mutex<Vec<Message>>>,
}

impl MicrocompactInspectingLlmClient {
    fn new(large_tool_payload: String) -> Self {
        Self {
            responses_seen: Arc::new(Mutex::new(0)),
            large_tool_payload,
            third_request_messages: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn third_request_messages(&self) -> Vec<Message> {
        self.third_request_messages
            .lock()
            .expect("messages poisoned")
            .clone()
    }
}

impl LlmClient for MicrocompactInspectingLlmClient {
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
                    "bash_large",
                    "bash",
                    BTreeMap::from([(
                        "command".to_string(),
                        json!(format!("printf '{}'", self.large_tool_payload)),
                    )]),
                )],
            ));
        }
        if *responses_seen == 2 {
            return Ok(LLMResponse::new(
                "continue once so older tool output can age",
            ));
        }
        *self
            .third_request_messages
            .lock()
            .expect("messages poisoned") = request.messages.clone();
        Ok(LLMResponse::with_tool_calls(
            "finish",
            vec![ToolCall::new(
                "finish_after_microcompact",
                "task_finish",
                BTreeMap::from([("message".to_string(), json!("memory compacted"))]),
            )],
        ))
    }
}
