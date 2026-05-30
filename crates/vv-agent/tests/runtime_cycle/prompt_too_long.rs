use super::*;

#[test]
fn runtime_retries_prompt_too_long_with_emergency_compaction() {
    let llm = PromptTooLongRetryLlmClient::default();
    let inspector = llm.clone();
    let runtime = AgentRuntime::new(llm);
    let mut task = AgentTask::new("ptl_task", "demo", "system", "finish after retry");
    task.no_tool_policy = vv_agent::NoToolPolicy::Finish;
    task.memory_compact_threshold = 10_000;
    task.metadata
        .insert("model_context_window".to_string(), json!(20_000));
    task.metadata
        .insert("reserved_output_tokens".to_string(), json!(0));
    task.metadata
        .insert("autocompact_buffer_tokens".to_string(), json!(0));
    task.metadata
        .insert("memory_keep_recent_messages".to_string(), json!(2));

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(result.final_answer.as_deref(), Some("done after retry"));
    assert!(result.cycles.iter().any(|cycle| cycle.memory_compacted));
    assert_eq!(inspector.request_count(), 3);
    let final_request = inspector.final_request_messages();
    assert!(
        final_request.len() < 4,
        "final retry should use emergency-compacted messages: {final_request:#?}"
    );
}

#[test]
fn runtime_returns_compaction_exhausted_after_prompt_too_long_retries() {
    let llm = AlwaysPromptTooLongLlmClient::default();
    let inspector = llm.clone();
    let runtime = AgentRuntime::new(llm);
    let mut task = AgentTask::new("ptl_exhausted", "demo", "system", "never fits");
    task.memory_compact_threshold = 10_000;
    task.metadata
        .insert("model_context_window".to_string(), json!(20_000));
    task.metadata
        .insert("reserved_output_tokens".to_string(), json!(0));
    task.metadata
        .insert("autocompact_buffer_tokens".to_string(), json!(0));

    let error = runtime.run(task).expect_err("compaction exhausted error");

    match error {
        LlmError::CompactionExhausted(error) => {
            assert_eq!(error.attempts, 4);
            assert!(error
                .last_error
                .as_deref()
                .is_some_and(|message| message.contains("Prompt is too long")));
        }
        other => panic!("expected CompactionExhausted, got {other:?}"),
    }
    assert_eq!(inspector.request_count(), 4);
}
#[derive(Clone, Default)]
struct PromptTooLongRetryLlmClient {
    requests_seen: Arc<Mutex<usize>>,
    final_request_messages: Arc<Mutex<Vec<Message>>>,
}

impl PromptTooLongRetryLlmClient {
    fn request_count(&self) -> usize {
        *self.requests_seen.lock().expect("request count poisoned")
    }

    fn final_request_messages(&self) -> Vec<Message> {
        self.final_request_messages
            .lock()
            .expect("messages poisoned")
            .clone()
    }
}

impl LlmClient for PromptTooLongRetryLlmClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        let mut requests_seen = self
            .requests_seen
            .lock()
            .map_err(|_| LlmError::Request("request count poisoned".to_string()))?;
        *requests_seen += 1;
        if *requests_seen <= 2 {
            return Err(LlmError::Request(
                "Prompt is too long for this model".to_string(),
            ));
        }
        *self
            .final_request_messages
            .lock()
            .expect("messages poisoned") = request.messages;
        Ok(LLMResponse::new("done after retry"))
    }
}

#[derive(Clone, Default)]
struct AlwaysPromptTooLongLlmClient {
    requests_seen: Arc<Mutex<usize>>,
}

impl AlwaysPromptTooLongLlmClient {
    fn request_count(&self) -> usize {
        *self.requests_seen.lock().expect("request count poisoned")
    }
}

impl LlmClient for AlwaysPromptTooLongLlmClient {
    fn complete(&self, _request: LlmRequest) -> Result<LLMResponse, LlmError> {
        let mut requests_seen = self
            .requests_seen
            .lock()
            .map_err(|_| LlmError::Request("request count poisoned".to_string()))?;
        *requests_seen += 1;
        Err(LlmError::Request(
            "Prompt is too long for this model".to_string(),
        ))
    }
}
