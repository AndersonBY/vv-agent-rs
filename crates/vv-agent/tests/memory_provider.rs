use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::json;
use vv_agent::{
    AgentRuntime, ExecutionContext, LLMResponse, LlmClient, LlmError, LlmRequest, MemoryFuture,
    MemoryProvider, MemoryProviderResult, MemorySaveRequest, MemorySaveResult, MemorySearchRequest,
    MemorySearchResult, RunEventPayload, RuntimeRunControls, ToolCall,
};

#[derive(Clone, Default)]
struct RecordingMemoryProvider {
    calls: Arc<Mutex<Vec<String>>>,
}

impl MemoryProvider for RecordingMemoryProvider {
    fn search(&self, _request: MemorySearchRequest) -> MemoryFuture<Vec<MemorySearchResult>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn save(&self, _request: MemorySaveRequest) -> MemoryFuture<MemorySaveResult> {
        Box::pin(async { Ok(MemorySaveResult::default()) })
    }

    fn before_compact(&self, event: &vv_agent::RunEvent) -> MemoryFuture<MemoryProviderResult> {
        assert!(matches!(
            event.payload(),
            RunEventPayload::MemoryCompactStarted { .. }
        ));
        let calls = self.calls.clone();
        Box::pin(async move {
            calls.lock().expect("lock").push("before".to_string());
            Ok(MemoryProviderResult::default())
        })
    }

    fn after_compact(&self, event: &vv_agent::RunEvent) -> MemoryFuture<()> {
        assert!(matches!(
            event.payload(),
            RunEventPayload::MemoryCompactCompleted { .. }
        ));
        let calls = self.calls.clone();
        Box::pin(async move {
            calls.lock().expect("lock").push("after".to_string());
            Ok(())
        })
    }
}

#[test]
fn memory_provider_receives_compaction_lifecycle_events() {
    let provider = RecordingMemoryProvider::default();
    let large_tool_payload = "tool output ".repeat(300);
    let llm = MemoryCompactionLlm::new(large_tool_payload);
    let mut runtime = AgentRuntime::new(llm);
    let workspace = tempfile::tempdir().expect("workspace");
    runtime.default_workspace = Some(workspace.path().to_path_buf());
    runtime.workspace_backend = Arc::new(vv_agent::LocalWorkspaceBackend::new(workspace.path()));

    let mut task = vv_agent::AgentTask::new("memory_provider_task", "demo", "system", "go");
    task.memory_compact_threshold = 20;
    task.metadata
        .insert("model_context_window".to_string(), json!(120));
    task.metadata
        .insert("reserved_output_tokens".to_string(), json!(10));
    task.metadata
        .insert("autocompact_buffer_tokens".to_string(), json!(0));

    let result = runtime
        .run_with_controls(
            task,
            RuntimeRunControls {
                execution_context: Some(ExecutionContext {
                    memory_providers: vec![Arc::new(provider.clone())],
                    ..ExecutionContext::default()
                }),
                ..RuntimeRunControls::default()
            },
        )
        .expect("run");

    assert_eq!(result.status, vv_agent::AgentStatus::Completed);
    assert_eq!(
        provider.calls.lock().expect("lock").as_slice(),
        &["before".to_string(), "after".to_string()]
    );
}

#[derive(Clone)]
struct MemoryCompactionLlm {
    responses_seen: Arc<Mutex<usize>>,
    large_tool_payload: String,
}

impl MemoryCompactionLlm {
    fn new(large_tool_payload: String) -> Self {
        Self {
            responses_seen: Arc::new(Mutex::new(0)),
            large_tool_payload,
        }
    }
}

impl LlmClient for MemoryCompactionLlm {
    fn complete(&self, _request: LlmRequest) -> Result<LLMResponse, LlmError> {
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
        Ok(LLMResponse::with_tool_calls(
            "finish",
            vec![ToolCall::new(
                "finish_after_compact",
                "task_finish",
                BTreeMap::from([("message".to_string(), json!("memory compacted"))]),
            )],
        ))
    }

    fn complete_with_stream(
        &self,
        request: LlmRequest,
        _stream_callback: Option<vv_agent::LlmStreamCallback>,
    ) -> Result<LLMResponse, LlmError> {
        self.complete(request)
    }
}
