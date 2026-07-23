use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use vv_agent::{
    Agent, AgentStatus, LLMResponse, LlmClient, LlmError, LlmRequest, LlmStreamCallback,
    ModelError, ModelProvider, ModelRef, ModelSettings, ResolvedModelConfig, RunConfig, Runner,
    VvLlmModelProvider,
};

use super::{live_enabled, live_settings_path};

#[derive(Clone)]
struct RecordingProvider {
    inner: VvLlmModelProvider,
    observations: Arc<Mutex<Vec<Value>>>,
}

impl RecordingProvider {
    fn new(inner: VvLlmModelProvider) -> Self {
        Self {
            inner,
            observations: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn observations(&self) -> Vec<Value> {
        self.observations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl ModelProvider for RecordingProvider {
    fn resolve(&self, model: &ModelRef) -> Result<ResolvedModelConfig, ModelError> {
        self.inner.resolve(model)
    }

    fn client(&self, resolved: &ResolvedModelConfig) -> Result<Arc<dyn LlmClient>, ModelError> {
        Ok(Arc::new(RecordingClient {
            inner: self.inner.client(resolved)?,
            observations: Arc::clone(&self.observations),
        }))
    }

    fn default_settings(&self, resolved: &ResolvedModelConfig) -> ModelSettings {
        self.inner.default_settings(resolved)
    }

    fn default_model_ref(&self) -> Option<ModelRef> {
        self.inner.default_model_ref()
    }
}

struct RecordingClient {
    inner: Arc<dyn LlmClient>,
    observations: Arc<Mutex<Vec<Value>>>,
}

impl RecordingClient {
    fn complete_and_record(
        &self,
        request: LlmRequest,
        stream_callback: Option<LlmStreamCallback>,
    ) -> Result<LLMResponse, LlmError> {
        let request_summary = json!({
            "model": request.model,
            "message_roles": request
                .messages
                .iter()
                .map(|message| format!("{:?}", message.role).to_ascii_lowercase())
                .collect::<Vec<_>>(),
            "message_count": request.messages.len(),
            "tool_count": request.tools.len(),
            "request_max_tokens": request
                .model_settings
                .as_ref()
                .and_then(|settings| settings.max_tokens),
        });
        let result = match stream_callback {
            Some(callback) => self.inner.complete_with_stream(request, Some(callback)),
            None => self.inner.complete(request),
        };
        let observation = match &result {
            Ok(response) => json!({
                "request": request_summary,
                "response_content_chars": response.content.chars().count(),
                "usage": {
                    "input_tokens": response.token_usage.input_tokens,
                    "output_tokens": response.token_usage.output_tokens,
                    "total_tokens": response.token_usage.total_tokens,
                    "cached_tokens": response.token_usage.cache_usage.read_input_tokens,
                },
            }),
            Err(error) => json!({
                "request": request_summary,
                "error_type": match error {
                    LlmError::ScriptExhausted => "script_exhausted",
                    LlmError::CompactionExhausted(_) => "compaction_exhausted",
                    LlmError::Request(_) => "request",
                },
            }),
        };
        self.observations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(observation);
        result
    }
}

impl LlmClient for RecordingClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        self.complete_and_record(request, None)
    }

    fn complete_with_stream(
        &self,
        request: LlmRequest,
        stream_callback: Option<LlmStreamCallback>,
    ) -> Result<LLMResponse, LlmError> {
        self.complete_and_record(request, stream_callback)
    }

    fn set_debug_dump_dir(&self, debug_dump_dir: &Path) {
        self.inner.set_debug_dump_dir(debug_dump_dir);
    }
}

#[tokio::test]
#[ignore = "live API call; run with VV_AGENT_RUN_LIVE_TESTS=1 cargo test --test live_deepseek -- --ignored"]
async fn live_deepseek_session_memory_probe_accounts_for_every_model_call() {
    if !live_enabled() {
        eprintln!("set VV_AGENT_RUN_LIVE_TESTS=1 to run live DeepSeek Agent/Runner tests");
        return;
    }

    let backend = std::env::var("VV_AGENT_LIVE_BACKEND").unwrap_or_else(|_| "deepseek".into());
    let model_name =
        std::env::var("VV_AGENT_LIVE_MODEL").unwrap_or_else(|_| "deepseek-v4-pro".into());
    let model = ModelRef::backend(backend.clone(), model_name);
    let provider = RecordingProvider::new(
        VvLlmModelProvider::from_settings_file(live_settings_path()).with_default_backend(backend),
    );
    let observer = provider.clone();
    let workspace = tempfile::tempdir().expect("workspace");
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(workspace.path())
        .build()
        .expect("runner");
    let agent = Agent::builder("deepseek-session-memory-probe")
        .instructions("Reply with one short sentence.")
        .model(model)
        .max_cycles(1)
        .no_tool_policy(vv_agent::NoToolPolicy::Finish)
        .build()
        .expect("agent");
    let result = runner
        .run_with_config(
            &agent,
            "Remember that the probe marker is cobalt, then acknowledge it.",
            RunConfig::builder()
                .max_cycles(1)
                .no_tool_policy(vv_agent::NoToolPolicy::Finish)
                .metadata("session_memory_enabled", json!(true))
                .metadata("session_memory_min_tokens", json!(1))
                .metadata("session_memory_min_text_messages", json!(1))
                .build(),
        )
        .await
        .expect("run live session memory probe");

    let memory_files = find_session_memory_files(workspace.path());
    let memory_entries = memory_files
        .first()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|content| serde_json::from_str::<Value>(&content).ok())
        .and_then(|value| value.get("entries").and_then(Value::as_array).cloned())
        .unwrap_or_default();
    let observations = observer.observations();
    println!(
        "{}",
        json!({
            "implementation": "rust",
            "model_calls": observations,
            "session_memory_files": memory_files.len(),
            "session_memory_entries": memory_entries.len(),
            "reported_model_call_count": result.token_usage().model_calls.len(),
            "reported_usage": {
                "input_tokens": result.token_usage().input_tokens,
                "output_tokens": result.token_usage().output_tokens,
                "total_tokens": result.token_usage().total_tokens,
                "cached_input_tokens": result.token_usage().cache_usage.read_input_tokens,
            },
        })
    );

    assert_eq!(result.status(), AgentStatus::Completed);
    assert_eq!(observations.len(), 2, "unexpected model call observations");
    assert_eq!(observations[0]["request"]["message_roles"], json!(["user"]));
    assert!(
        !memory_files.is_empty(),
        "session memory file was not created"
    );
    assert!(
        !memory_entries.is_empty(),
        "session memory extraction was empty"
    );
    assert_eq!(result.token_usage().model_calls.len(), 2);
}

fn find_session_memory_files(workspace: &Path) -> Vec<PathBuf> {
    let root = workspace.join(".memory/session");
    let Ok(scopes) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut files = scopes
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("session_memory.json"))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    files.sort();
    files
}
