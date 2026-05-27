use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use futures_util::stream;
use serde_json::{json, Value};
use vv_agent::{
    AgentDefinition, AgentRuntime, AgentSDKClient, AgentSDKOptions, AgentStatus, AgentTask,
    ExecutionContext, LlmClient, LlmRequest, Message, RuntimeRunControls, StreamCallback,
    VvLlmClient,
};

#[derive(Clone, Default)]
struct StreamingChatClient {
    completion_calls: Arc<Mutex<u32>>,
    stream_calls: Arc<Mutex<u32>>,
}

impl StreamingChatClient {
    fn completion_calls(&self) -> u32 {
        *self.completion_calls.lock().expect("completion calls lock")
    }

    fn stream_calls(&self) -> u32 {
        *self.stream_calls.lock().expect("stream calls lock")
    }
}

impl vv_llm::ChatClient for StreamingChatClient {
    fn provider_name(&self) -> &'static str {
        "test-streaming"
    }

    fn create_completion<'life0, 'async_trait>(
        &'life0 self,
        request: vv_llm::ChatRequest,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<vv_llm::ChatResponse, vv_llm::VvLlmError>>
                + Send
                + 'async_trait,
        >,
    >
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        let completion_calls = Arc::clone(&self.completion_calls);
        Box::pin(async move {
            *completion_calls.lock().expect("completion calls lock") += 1;
            Ok(vv_llm::ChatResponse {
                id: "non-stream-response".to_string(),
                model: request.model,
                content: String::new(),
                tool_calls: vec![vv_llm::ToolCall::function(
                    "call_nonstream",
                    "task_finish",
                    r#"{"message":"non-stream fallback"}"#,
                )],
                usage: None,
            })
        })
    }

    fn create_stream<'life0, 'async_trait>(
        &'life0 self,
        request: vv_llm::ChatRequest,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<vv_llm::ChatStream, vv_llm::VvLlmError>>
                + Send
                + 'async_trait,
        >,
    >
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        let stream_calls = Arc::clone(&self.stream_calls);
        Box::pin(async move {
            *stream_calls.lock().expect("stream calls lock") += 1;
            assert_eq!(request.options.stream, Some(true));
            let deltas = vec![
                Ok(vv_llm::ChatStreamDelta {
                    content: "streamed ".to_string(),
                    ..vv_llm::ChatStreamDelta::default()
                }),
                Ok(vv_llm::ChatStreamDelta {
                    content: "content".to_string(),
                    ..vv_llm::ChatStreamDelta::default()
                }),
                Ok(vv_llm::ChatStreamDelta {
                    tool_calls: vec![vv_llm::ToolCall::function(
                        "call_stream",
                        "task_finish",
                        r#"{"message":"streamed answer"}"#,
                    )],
                    ..vv_llm::ChatStreamDelta::default()
                }),
                Ok(vv_llm::ChatStreamDelta {
                    usage: Some(vv_llm::ChatUsage {
                        prompt_tokens: Some(3),
                        completion_tokens: Some(5),
                        total_tokens: Some(8),
                    }),
                    done: true,
                    ..vv_llm::ChatStreamDelta::default()
                }),
            ];
            let chat_stream: vv_llm::ChatStream = Box::pin(stream::iter(deltas));
            Ok(chat_stream)
        })
    }
}

#[test]
fn runtime_execution_context_stream_callback_uses_vv_llm_streaming() {
    let chat_client = StreamingChatClient::default();
    let probe = chat_client.clone();
    let llm = VvLlmClient::new(
        "deepseek",
        "deepseek-v4-pro",
        "deepseek-v4-pro",
        Box::new(chat_client),
        90.0,
    );
    let runtime = AgentRuntime::new(llm);
    let events = Arc::new(Mutex::new(Vec::<BTreeMap<String, Value>>::new()));
    let callback_events = Arc::clone(&events);
    let stream_callback: StreamCallback = Arc::new(move |event| {
        callback_events
            .lock()
            .expect("stream events lock")
            .push(event.clone());
    });

    let result = runtime
        .run_with_controls(
            AgentTask::new("stream_ctx_task", "demo", "system", "finish via stream"),
            RuntimeRunControls {
                execution_context: Some(
                    ExecutionContext::default().with_stream_callback(stream_callback),
                ),
                ..RuntimeRunControls::default()
            },
        )
        .expect("streaming run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(result.final_answer.as_deref(), Some("streamed answer"));
    assert_eq!(probe.completion_calls(), 0);
    assert_eq!(probe.stream_calls(), 1);
    let events = events.lock().expect("stream events lock");
    assert!(events.iter().any(|event| {
        event.get("event") == Some(&json!("assistant_delta"))
            && event.get("content_delta") == Some(&json!("streamed "))
    }));
}

#[test]
fn sdk_options_stream_callback_is_forwarded_to_runtime() {
    let chat_client = StreamingChatClient::default();
    let probe = chat_client.clone();
    let runtime = AgentRuntime::new(VvLlmClient::new(
        "deepseek",
        "deepseek-v4-pro",
        "deepseek-v4-pro",
        Box::new(chat_client),
        90.0,
    ));
    let events = Arc::new(Mutex::new(Vec::<BTreeMap<String, Value>>::new()));
    let callback_events = Arc::clone(&events);
    let stream_callback: StreamCallback = Arc::new(move |event| {
        callback_events
            .lock()
            .expect("stream events lock")
            .push(event.clone());
    });
    let client = AgentSDKClient::new(AgentSDKOptions {
        stream_callback: Some(stream_callback),
        ..AgentSDKOptions::default()
    })
    .with_runtime(runtime);

    let run = client
        .run_with_agent(
            AgentDefinition::default_for_model("deepseek-v4-pro"),
            "finish via SDK stream",
        )
        .expect("sdk run");

    assert_eq!(run.result.status, AgentStatus::Completed);
    assert_eq!(run.result.final_answer.as_deref(), Some("streamed answer"));
    assert_eq!(probe.completion_calls(), 0);
    assert_eq!(probe.stream_calls(), 1);
    assert!(!events.lock().expect("stream events lock").is_empty());
}

#[test]
fn vv_llm_client_estimates_usage_when_provider_omits_usage_like_python() {
    let llm = VvLlmClient::new(
        "openai",
        "demo-model",
        "demo-model",
        Box::new(UsageMissingChatClient),
        90.0,
    );

    let response = llm
        .complete(LlmRequest::new(
            "demo-model",
            vec![Message::user("hello from usage estimator")],
        ))
        .expect("completion without provider usage");

    assert_eq!(response.content, "estimated usage response");
    assert!(response.token_usage.prompt_tokens > 0);
    assert!(response.token_usage.completion_tokens > 0);
    assert_eq!(
        response.token_usage.total_tokens,
        response.token_usage.prompt_tokens + response.token_usage.completion_tokens
    );
    assert_eq!(
        response.raw["usage"]["prompt_tokens"],
        json!(response.token_usage.prompt_tokens)
    );
    assert_eq!(
        response.raw["usage"]["completion_tokens"],
        json!(response.token_usage.completion_tokens)
    );
}

#[test]
fn vv_llm_client_auto_streams_deepseek_v4_models_like_python() {
    let chat_client = StreamingChatClient::default();
    let probe = chat_client.clone();
    let llm = VvLlmClient::new(
        "deepseek",
        "deepseek-v4-pro",
        "deepseek-v4-pro",
        Box::new(chat_client),
        90.0,
    );

    let response = llm
        .complete(LlmRequest::new(
            "deepseek-v4-pro",
            vec![Message::user("finish via automatic stream")],
        ))
        .expect("automatic streaming completion");

    assert_eq!(response.content, "streamed content");
    assert_eq!(response.tool_calls[0].name, "task_finish");
    assert_eq!(probe.completion_calls(), 0);
    assert_eq!(probe.stream_calls(), 1);
}

#[test]
fn vv_llm_client_fails_over_to_next_endpoint_client_like_python() {
    let llm = VvLlmClient::new_with_named_endpoint_clients(
        "openai",
        "gpt-4o-alias",
        "gpt-4o-mini",
        vec![
            (
                "primary-endpoint".to_string(),
                "gpt-4o-mini".to_string(),
                Box::new(FailingChatClient) as Box<dyn vv_llm::ChatClient>,
            ),
            (
                "backup-endpoint".to_string(),
                "gpt-4o-mini-backup".to_string(),
                Box::new(UsageMissingChatClient) as Box<dyn vv_llm::ChatClient>,
            ),
        ],
        90.0,
    );

    let response = llm
        .complete(LlmRequest::new(
            "gpt-4o-alias",
            vec![Message::user("fall over to backup endpoint")],
        ))
        .expect("completion from fallback endpoint");

    assert_eq!(response.content, "estimated usage response");
    assert_eq!(response.raw["used_endpoint_id"], json!("backup-endpoint"));
    assert_eq!(response.raw["used_model_id"], json!("gpt-4o-mini-backup"));
    assert_eq!(response.raw["stream_mode"], json!(false));
}

#[test]
fn vv_llm_client_uses_endpoint_model_for_selected_alias_like_python() {
    let llm = VvLlmClient::new_with_named_endpoint_clients(
        "openai",
        "gpt-alias",
        "gpt-provider-model",
        vec![(
            "primary-endpoint".to_string(),
            "gpt-provider-model".to_string(),
            Box::new(UsageMissingChatClient) as Box<dyn vv_llm::ChatClient>,
        )],
        90.0,
    );

    let response = llm
        .complete(LlmRequest::new(
            "gpt-alias",
            vec![Message::user("use provider model id")],
        ))
        .expect("completion from primary endpoint");

    assert_eq!(response.raw["used_model_id"], json!("gpt-provider-model"));
}

#[test]
fn vv_llm_client_converts_extra_minimax_system_messages_like_python() {
    let chat_client = RecordingMessagesChatClient::default();
    let probe = chat_client.clone();
    let llm = VvLlmClient::new(
        "minimax",
        "MiniMax-M2.5",
        "MiniMax-M2.5",
        Box::new(chat_client),
        90.0,
    );
    let mut memory_summary = Message::system("summary");
    memory_summary.name = Some("memory_summary".to_string());

    let _ = llm
        .complete(LlmRequest::new(
            "MiniMax-M2.5",
            vec![
                Message::system("base system"),
                memory_summary,
                Message::assistant("next"),
            ],
        ))
        .expect("minimax request");

    let messages = probe.messages();
    assert_eq!(messages[0].role, vv_llm::MessageRole::System);
    assert_eq!(messages[0].text_content().as_deref(), Some("base system"));
    assert_eq!(messages[1].role, vv_llm::MessageRole::User);
    assert_eq!(
        messages[1].text_content().as_deref(),
        Some("[memory_summary]\nsummary")
    );
    assert_eq!(messages[2].role, vv_llm::MessageRole::Assistant);
}

#[test]
fn vv_llm_client_applies_deepseek_reasoning_temperature_like_python() {
    let chat_client = RecordingMessagesChatClient::default();
    let probe = chat_client.clone();
    let llm = VvLlmClient::new(
        "deepseek",
        "deepseek-v4-pro",
        "deepseek-v4-pro",
        Box::new(chat_client),
        90.0,
    );

    let _ = llm
        .complete(LlmRequest::new(
            "deepseek-v4-pro",
            vec![Message::user("use reasoning temp")],
        ))
        .expect("deepseek request");

    let request = probe.last_request().expect("recorded request");
    assert_eq!(request.options.temperature, Some(0.6));
}

#[test]
fn vv_llm_client_normalizes_supported_thinking_model_options_like_python() {
    let claude_client = RecordingMessagesChatClient::default();
    let claude_probe = claude_client.clone();
    let claude = VvLlmClient::new(
        "anthropic",
        "claude-opus-4-6-thinking",
        "claude-opus-4-6-thinking",
        Box::new(claude_client),
        90.0,
    );
    let _ = claude
        .complete(LlmRequest::new(
            "claude-opus-4-6-thinking",
            vec![Message::user("think")],
        ))
        .expect("claude thinking request");

    let claude_request = claude_probe.last_request().expect("claude request");
    assert_eq!(claude_request.model, "claude-opus-4-6");
    assert_eq!(claude_request.options.temperature, Some(1.0));
    assert_eq!(claude_request.options.max_tokens, Some(20_000));

    let gemini_client = RecordingMessagesChatClient::default();
    let gemini_probe = gemini_client.clone();
    let gemini = VvLlmClient::new(
        "gemini",
        "gemini-3-pro",
        "gemini-3-pro",
        Box::new(gemini_client),
        90.0,
    );
    let _ = gemini
        .complete(LlmRequest::new(
            "gemini-3-pro",
            vec![Message::user("think")],
        ))
        .expect("gemini thinking request");

    let gemini_request = gemini_probe.last_request().expect("gemini request");
    assert_eq!(gemini_request.model, "gemini-3-pro-preview");
    assert_eq!(gemini_request.options.temperature, Some(1.0));
}

#[derive(Clone, Default)]
struct RecordingMessagesChatClient {
    requests: Arc<Mutex<Vec<vv_llm::ChatRequest>>>,
}

impl RecordingMessagesChatClient {
    fn messages(&self) -> Vec<vv_llm::Message> {
        self.last_request()
            .map(|request| request.messages)
            .unwrap_or_default()
    }

    fn last_request(&self) -> Option<vv_llm::ChatRequest> {
        self.requests
            .lock()
            .expect("recorded requests lock")
            .last()
            .cloned()
    }
}

impl vv_llm::ChatClient for RecordingMessagesChatClient {
    fn provider_name(&self) -> &'static str {
        "recording"
    }

    fn create_completion<'life0, 'async_trait>(
        &'life0 self,
        request: vv_llm::ChatRequest,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<vv_llm::ChatResponse, vv_llm::VvLlmError>>
                + Send
                + 'async_trait,
        >,
    >
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        let requests = Arc::clone(&self.requests);
        Box::pin(async move {
            requests
                .lock()
                .expect("recorded requests lock")
                .push(request.clone());
            Ok(vv_llm::ChatResponse {
                id: "recording-response".to_string(),
                model: request.model,
                content: "recorded".to_string(),
                tool_calls: Vec::new(),
                usage: None,
            })
        })
    }

    fn create_stream<'life0, 'async_trait>(
        &'life0 self,
        request: vv_llm::ChatRequest,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<vv_llm::ChatStream, vv_llm::VvLlmError>>
                + Send
                + 'async_trait,
        >,
    >
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        let requests = Arc::clone(&self.requests);
        Box::pin(async move {
            requests
                .lock()
                .expect("recorded requests lock")
                .push(request);
            let chat_stream: vv_llm::ChatStream =
                Box::pin(stream::iter([Ok(vv_llm::ChatStreamDelta {
                    content: "recorded".to_string(),
                    done: true,
                    ..vv_llm::ChatStreamDelta::default()
                })]));
            Ok(chat_stream)
        })
    }
}

#[derive(Clone, Default)]
struct FailingChatClient;

impl vv_llm::ChatClient for FailingChatClient {
    fn provider_name(&self) -> &'static str {
        "failing"
    }

    fn create_completion<'life0, 'async_trait>(
        &'life0 self,
        _request: vv_llm::ChatRequest,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<vv_llm::ChatResponse, vv_llm::VvLlmError>>
                + Send
                + 'async_trait,
        >,
    >
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move { Err(vv_llm::VvLlmError::Provider("primary down".to_string())) })
    }

    fn create_stream<'life0, 'async_trait>(
        &'life0 self,
        _request: vv_llm::ChatRequest,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<vv_llm::ChatStream, vv_llm::VvLlmError>>
                + Send
                + 'async_trait,
        >,
    >
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move { Err(vv_llm::VvLlmError::Provider("primary down".to_string())) })
    }
}

#[derive(Clone, Default)]
struct UsageMissingChatClient;

impl vv_llm::ChatClient for UsageMissingChatClient {
    fn provider_name(&self) -> &'static str {
        "usage-missing"
    }

    fn create_completion<'life0, 'async_trait>(
        &'life0 self,
        request: vv_llm::ChatRequest,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<vv_llm::ChatResponse, vv_llm::VvLlmError>>
                + Send
                + 'async_trait,
        >,
    >
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            Ok(vv_llm::ChatResponse {
                id: "usage-missing-response".to_string(),
                model: request.model,
                content: "estimated usage response".to_string(),
                tool_calls: Vec::new(),
                usage: None,
            })
        })
    }

    fn create_stream<'life0, 'async_trait>(
        &'life0 self,
        _request: vv_llm::ChatRequest,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<vv_llm::ChatStream, vv_llm::VvLlmError>>
                + Send
                + 'async_trait,
        >,
    >
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            let chat_stream: vv_llm::ChatStream = Box::pin(stream::empty());
            Ok(chat_stream)
        })
    }
}
