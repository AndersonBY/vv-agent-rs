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
                reasoning_content: None,
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

#[derive(Clone, Default)]
struct UnicodeStreamingChatClient;

impl vv_llm::ChatClient for UnicodeStreamingChatClient {
    fn provider_name(&self) -> &'static str {
        "test-unicode-streaming"
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
        Box::pin(async move {
            Ok(vv_llm::ChatResponse {
                id: "unicode-non-stream-response".to_string(),
                model: "deepseek-v4-pro".to_string(),
                content: "not streamed".to_string(),
                tool_calls: vec![],
                reasoning_content: None,
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
            let deltas = vec![
                Ok(vv_llm::ChatStreamDelta {
                    reasoning_content: "思考".to_string(),
                    ..vv_llm::ChatStreamDelta::default()
                }),
                Ok(vv_llm::ChatStreamDelta {
                    content: "你好世界".to_string(),
                    done: true,
                    ..vv_llm::ChatStreamDelta::default()
                }),
            ];
            Ok(Box::pin(stream::iter(deltas)) as vv_llm::ChatStream)
        })
    }
}

#[test]
fn runtime_execution_context_stream_callback_uses_vv_llm_streaming() {
    let chat_client = StreamingChatClient::default();
    let probe = chat_client.clone();
    let llm = VvLlmClient::new(
        "deepseek",
        "deepseek-chat",
        "deepseek-chat",
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
fn structured_stream_events_estimate_tokens_from_char_count_like_python() {
    let llm = VvLlmClient::new(
        "deepseek",
        "deepseek-v4-pro",
        "deepseek-v4-pro",
        Box::new(UnicodeStreamingChatClient),
        90.0,
    );
    let events = Arc::new(Mutex::new(Vec::<BTreeMap<String, Value>>::new()));
    let callback_events = Arc::clone(&events);
    let stream_callback: StreamCallback = Arc::new(move |event| {
        callback_events
            .lock()
            .expect("stream events lock")
            .push(event.clone());
    });

    let response = llm
        .complete_with_stream(
            LlmRequest::new(
                "deepseek-v4-pro",
                vec![Message::user("stream unicode content")],
            ),
            Some(stream_callback),
        )
        .expect("unicode streaming completion");

    assert_eq!(response.content, "你好世界");
    assert_eq!(response.raw["reasoning_content"], json!("思考"));

    let events = events.lock().expect("stream events lock");
    let reasoning = events
        .iter()
        .find(|event| event.get("event") == Some(&json!("reasoning_delta")))
        .expect("reasoning delta");
    assert_eq!(reasoning["reasoning_chars"], json!(2));
    assert_eq!(reasoning["estimated_tokens"], json!(1));

    let content = events
        .iter()
        .find(|event| event.get("event") == Some(&json!("assistant_delta")))
        .expect("assistant delta");
    assert_eq!(content["content_chars"], json!(4));
    assert_eq!(content["estimated_tokens"], json!(1));
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
    )
    .with_randomize_endpoints(false)
    .with_retry_policy(1, 0.0);

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
fn vv_llm_client_prefers_last_successful_endpoint_like_python() {
    let primary = CountingFailingChatClient::default();
    let primary_probe = primary.clone();
    let llm = VvLlmClient::new_with_named_endpoint_clients(
        "openai",
        "gpt-4o-mini",
        "gpt-4o-mini",
        vec![
            (
                "primary-endpoint".to_string(),
                "gpt-4o-mini".to_string(),
                Box::new(primary) as Box<dyn vv_llm::ChatClient>,
            ),
            (
                "backup-endpoint".to_string(),
                "gpt-4o-mini".to_string(),
                Box::new(UsageMissingChatClient) as Box<dyn vv_llm::ChatClient>,
            ),
        ],
        90.0,
    )
    .with_randomize_endpoints(false)
    .with_retry_policy(1, 0.0);

    let first = llm
        .complete(LlmRequest::new("gpt-4o-mini", vec![Message::user("first")]))
        .expect("first fallback completion");
    let second = llm
        .complete(LlmRequest::new(
            "gpt-4o-mini",
            vec![Message::user("second")],
        ))
        .expect("second preferred completion");

    assert_eq!(first.raw["used_endpoint_id"], json!("backup-endpoint"));
    assert_eq!(second.raw["used_endpoint_id"], json!("backup-endpoint"));
    assert_eq!(primary_probe.calls(), 1);
}

#[test]
fn vv_llm_client_exposes_python_style_endpoint_randomization_policy() {
    let llm = VvLlmClient::new_with_named_endpoint_clients(
        "openai",
        "gpt-4o-mini",
        "gpt-4o-mini",
        vec![
            (
                "primary-endpoint".to_string(),
                "gpt-4o-mini".to_string(),
                Box::new(UsageMissingChatClient) as Box<dyn vv_llm::ChatClient>,
            ),
            (
                "backup-endpoint".to_string(),
                "gpt-4o-mini".to_string(),
                Box::new(UsageMissingChatClient) as Box<dyn vv_llm::ChatClient>,
            ),
        ],
        90.0,
    );

    assert!(llm.randomize_endpoints());
    let llm = llm.with_randomize_endpoints(false);
    assert!(!llm.randomize_endpoints());
}

#[test]
fn vv_llm_client_retries_endpoint_before_failover_like_python() {
    let flaky = FlakyChatClient::new(1);
    let flaky_probe = flaky.clone();
    let llm = VvLlmClient::new(
        "openai",
        "gpt-4o-mini",
        "gpt-4o-mini",
        Box::new(flaky),
        90.0,
    )
    .with_retry_policy(2, 0.0);

    let response = llm
        .complete(LlmRequest::new(
            "gpt-4o-mini",
            vec![Message::user("retry once")],
        ))
        .expect("retry succeeds");

    assert_eq!(response.content, "flaky success");
    assert_eq!(flaky_probe.calls(), 2);
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
fn vv_llm_client_omits_empty_optional_request_fields_like_python() {
    let chat_client = RecordingMessagesChatClient::default();
    let probe = chat_client.clone();
    let llm = VvLlmClient::new(
        "openai",
        "demo-model",
        "demo-model",
        Box::new(chat_client),
        90.0,
    );
    let mut user = Message::user("inspect");
    user.name = Some(String::new());
    user.tool_call_id = Some(String::new());
    user.image_url = Some(String::new());

    let _ = llm
        .complete(LlmRequest::new("demo-model", vec![user]))
        .expect("request with empty optional fields");

    let messages = probe.messages();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].name, None);
    assert_eq!(messages[0].tool_call_id, None);
    assert_eq!(
        messages[0].content,
        vec![vv_llm::MessageContent::Text {
            text: "inspect".to_string(),
        }]
    );
}

#[test]
fn vv_llm_client_preserves_reasoning_and_tool_extra_content_through_vv_llm() {
    let chat_client = RecordingMessagesChatClient::default();
    let probe = chat_client.clone();
    let llm = VvLlmClient::new(
        "deepseek",
        "deepseek-v4-pro",
        "deepseek-v4-pro",
        Box::new(chat_client),
        90.0,
    );
    let mut assistant = Message::assistant("");
    assistant.reasoning_content = Some("old-thought".to_string());
    let mut call = vv_agent::ToolCall::new(
        "call_1",
        "default_api:list_files",
        [("path".to_string(), json!("."))].into_iter().collect(),
    );
    call.extra_content = Some(json!({"google": {"thought_signature": "sig_123"}}));
    assistant.tool_calls = vec![call];

    let response = llm
        .complete(LlmRequest::new(
            "deepseek-chat",
            vec![Message::user("continue"), assistant],
        ))
        .expect("vv-llm request");

    let request = probe.last_request().expect("recorded request");
    let assistant = request
        .messages
        .iter()
        .find(|message| message.role == vv_llm::MessageRole::Assistant)
        .expect("assistant request message");
    assert_eq!(assistant.reasoning_content.as_deref(), Some("old-thought"));
    assert_eq!(
        assistant.tool_calls[0]
            .extra_content
            .as_ref()
            .expect("extra content")["google"]["thought_signature"],
        json!("sig_123")
    );
    assert_eq!(response.raw["reasoning_content"], json!("new-thought"));
    assert_eq!(
        response.tool_calls[0]
            .extra_content
            .as_ref()
            .expect("response extra content")["google"]["thought_signature"],
        json!("sig_456")
    );
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
    assert_eq!(
        claude_request.extra_body["thinking"],
        json!({"type": "enabled", "budget_tokens": 16000})
    );

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
    assert_eq!(
        gemini_request.extra_body["extra_body"]["google"]["thinking_config"]["thinkingLevel"],
        json!("high")
    );
    assert_eq!(
        gemini_request.extra_body["extra_body"]["google"]["thinking_config"]["include_thoughts"],
        json!(true)
    );
}

#[test]
fn vv_llm_client_normalizes_more_provider_model_aliases_like_python() {
    let qwen_client = RecordingMessagesChatClient::default();
    let qwen_probe = qwen_client.clone();
    let qwen = VvLlmClient::new(
        "qwen",
        "qwen3-32b-thinking",
        "qwen3-32b-thinking",
        Box::new(qwen_client),
        90.0,
    );
    let _ = qwen
        .complete(LlmRequest::new(
            "qwen3-32b-thinking",
            vec![Message::user("think")],
        ))
        .expect("qwen thinking request");
    assert_eq!(
        qwen_probe.last_request().expect("qwen request").model,
        "qwen3-32b"
    );
    assert_eq!(
        qwen_probe.last_request().expect("qwen request").extra_body["enable_thinking"],
        json!(true)
    );

    let qwen_keep_client = RecordingMessagesChatClient::default();
    let qwen_keep_probe = qwen_keep_client.clone();
    let qwen_keep = VvLlmClient::new(
        "qwen",
        "qwen3-next-80b-a3b-thinking",
        "qwen3-next-80b-a3b-thinking",
        Box::new(qwen_keep_client),
        90.0,
    );
    let _ = qwen_keep
        .complete(LlmRequest::new(
            "qwen3-next-80b-a3b-thinking",
            vec![Message::user("keep suffix")],
        ))
        .expect("qwen keep suffix request");
    assert_eq!(
        qwen_keep_probe
            .last_request()
            .expect("qwen keep request")
            .model,
        "qwen3-next-80b-a3b-thinking"
    );

    let glm_client = RecordingMessagesChatClient::default();
    let glm_probe = glm_client.clone();
    let glm = VvLlmClient::new(
        "zhipuai",
        "glm-5-air-thinking",
        "glm-5-air-thinking",
        Box::new(glm_client),
        90.0,
    );
    let _ = glm
        .complete(LlmRequest::new(
            "glm-5-air-thinking",
            vec![Message::user("think")],
        ))
        .expect("glm thinking request");
    assert_eq!(
        glm_probe.last_request().expect("glm request").model,
        "glm-5-air"
    );
    assert_eq!(
        glm_probe.last_request().expect("glm request").extra_body["thinking"],
        json!({"type": "enabled"})
    );

    let gpt_client = RecordingMessagesChatClient::default();
    let gpt_probe = gpt_client.clone();
    let gpt = VvLlmClient::new(
        "openai",
        "gpt-5-high",
        "gpt-5-high",
        Box::new(gpt_client),
        90.0,
    );
    let _ = gpt
        .complete(LlmRequest::new(
            "gpt-5-high",
            vec![Message::user("high effort")],
        ))
        .expect("gpt high request");
    assert_eq!(
        gpt_probe.last_request().expect("gpt request").model,
        "gpt-5"
    );
    assert_eq!(
        gpt_probe.last_request().expect("gpt request").extra_body["reasoning_effort"],
        json!("high")
    );

    let o3_client = RecordingMessagesChatClient::default();
    let o3_probe = o3_client.clone();
    let o3 = VvLlmClient::new(
        "openai",
        "o3-mini-high",
        "o3-mini-high",
        Box::new(o3_client),
        90.0,
    );
    let _ = o3
        .complete(LlmRequest::new(
            "o3-mini-high",
            vec![Message::user("high effort")],
        ))
        .expect("o3 high request");
    assert_eq!(
        o3_probe.last_request().expect("o3 request").model,
        "o3-mini"
    );
    assert_eq!(
        o3_probe.last_request().expect("o3 request").extra_body["reasoning_effort"],
        json!("high")
    );
}

#[test]
fn vv_llm_client_normalizes_tool_call_ids_and_names_like_python() {
    let llm = VvLlmClient::new(
        "openai",
        "demo-model",
        "demo-model",
        Box::new(UnnormalizedToolCallChatClient),
        90.0,
    );

    let response = llm
        .complete(LlmRequest::new(
            "demo-model",
            vec![Message::user("call a tool")],
        ))
        .expect("tool call response");

    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].name, "task_finish");
    assert!(!response.tool_calls[0].id.is_empty());
    assert_eq!(response.tool_calls[0].arguments["message"], json!("done"));
}

#[test]
fn vv_llm_stream_collects_raw_content_blocks_like_python() {
    let llm = VvLlmClient::new(
        "moonshot",
        "kimi-k2.5",
        "kimi-k2.5",
        Box::new(RawContentChatClient),
        90.0,
    );

    let response = llm
        .complete(LlmRequest::new(
            "kimi-k2.5",
            vec![Message::user("collect raw blocks")],
        ))
        .expect("raw content stream");

    assert_eq!(response.content, "done");
    let raw_content = response.raw["raw_content"]
        .as_array()
        .expect("raw content array");
    assert_eq!(raw_content[0]["type"], json!("thinking"));
    assert_eq!(raw_content[0]["thinking"], json!("step-1"));
    assert_eq!(raw_content[0]["signature"], json!("sig-1"));
    assert_eq!(raw_content[1]["type"], json!("text"));
    assert_eq!(raw_content[1]["text"], json!("visible text"));
}

#[test]
fn vv_llm_client_debug_dump_writes_request_messages_like_python() {
    let dump_dir = tempfile::tempdir().expect("dump dir");
    let chat_client = RecordingMessagesChatClient::default();
    let llm = VvLlmClient::new(
        "openai",
        "gpt/4o-mini",
        "gpt/4o-mini",
        Box::new(chat_client),
        90.0,
    )
    .with_debug_dump_dir(dump_dir.path());

    let response = llm
        .complete(LlmRequest::new("gpt/4o-mini", vec![Message::user("hello")]))
        .expect("debug dump request");

    assert_eq!(response.content, "recorded");
    let dump_files = std::fs::read_dir(dump_dir.path())
        .expect("read dump dir")
        .map(|entry| entry.expect("dump entry").file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(dump_files, vec!["request_001_gpt_4o-mini.json"]);
    let payload =
        std::fs::read_to_string(dump_dir.path().join(&dump_files[0])).expect("read dump payload");
    assert!(payload.contains("\"request_index\": 1"));
    assert!(payload.contains("\"message_count\": 1"));
}

#[derive(Clone, Default)]
struct CountingFailingChatClient {
    calls: Arc<Mutex<u32>>,
}

impl CountingFailingChatClient {
    fn calls(&self) -> u32 {
        *self.calls.lock().expect("counting calls lock")
    }
}

impl vv_llm::ChatClient for CountingFailingChatClient {
    fn provider_name(&self) -> &'static str {
        "counting-failing"
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
        let calls = Arc::clone(&self.calls);
        Box::pin(async move {
            *calls.lock().expect("counting calls lock") += 1;
            Err(vv_llm::VvLlmError::Provider("primary down".to_string()))
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
        let calls = Arc::clone(&self.calls);
        Box::pin(async move {
            *calls.lock().expect("counting calls lock") += 1;
            Err(vv_llm::VvLlmError::Provider("primary down".to_string()))
        })
    }
}

#[derive(Clone)]
struct FlakyChatClient {
    failures_remaining: Arc<Mutex<u32>>,
    calls: Arc<Mutex<u32>>,
}

impl FlakyChatClient {
    fn new(failures_remaining: u32) -> Self {
        Self {
            failures_remaining: Arc::new(Mutex::new(failures_remaining)),
            calls: Arc::new(Mutex::new(0)),
        }
    }

    fn calls(&self) -> u32 {
        *self.calls.lock().expect("flaky calls lock")
    }
}

impl vv_llm::ChatClient for FlakyChatClient {
    fn provider_name(&self) -> &'static str {
        "flaky"
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
        let failures_remaining = Arc::clone(&self.failures_remaining);
        let calls = Arc::clone(&self.calls);
        Box::pin(async move {
            *calls.lock().expect("flaky calls lock") += 1;
            let mut failures = failures_remaining.lock().expect("flaky failures lock");
            if *failures > 0 {
                *failures -= 1;
                return Err(vv_llm::VvLlmError::Provider(
                    "transient endpoint error".to_string(),
                ));
            }
            Ok(vv_llm::ChatResponse {
                id: "flaky-response".to_string(),
                model: request.model,
                content: "flaky success".to_string(),
                tool_calls: Vec::new(),
                reasoning_content: None,
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

#[derive(Clone, Default)]
struct RawContentChatClient;

impl vv_llm::ChatClient for RawContentChatClient {
    fn provider_name(&self) -> &'static str {
        "raw-content"
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
        Box::pin(async move {
            Ok(vv_llm::ChatResponse {
                id: "raw-content-response".to_string(),
                model: "kimi-k2.5".to_string(),
                content: String::new(),
                tool_calls: Vec::new(),
                reasoning_content: None,
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
            let deltas = vec![
                Ok(vv_llm::ChatStreamDelta {
                    raw_content: Some(json!({"type": "thinking_delta", "thinking": "step-1"})),
                    ..vv_llm::ChatStreamDelta::default()
                }),
                Ok(vv_llm::ChatStreamDelta {
                    raw_content: Some(json!({"type": "signature_delta", "signature": "sig-1"})),
                    ..vv_llm::ChatStreamDelta::default()
                }),
                Ok(vv_llm::ChatStreamDelta {
                    content: "done".to_string(),
                    raw_content: Some(json!({"type": "text_delta", "text": "visible text"})),
                    done: true,
                    ..vv_llm::ChatStreamDelta::default()
                }),
            ];
            let chat_stream: vv_llm::ChatStream = Box::pin(stream::iter(deltas));
            Ok(chat_stream)
        })
    }
}

#[derive(Clone, Default)]
struct UnnormalizedToolCallChatClient;

impl vv_llm::ChatClient for UnnormalizedToolCallChatClient {
    fn provider_name(&self) -> &'static str {
        "unnormalized-tool-call"
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
                id: "unnormalized-response".to_string(),
                model: request.model,
                content: String::new(),
                tool_calls: vec![vv_llm::ToolCall::function(
                    "",
                    "task _finish",
                    r#"{"message":"done"}"#,
                )],
                reasoning_content: None,
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
                tool_calls: vec![vv_llm::ToolCall {
                    id: "call_response".to_string(),
                    name: "default_api:read_file".to_string(),
                    arguments: r#"{"path":"README.md"}"#.to_string(),
                    extra_content: Some(json!({"google": {"thought_signature": "sig_456"}})),
                }],
                reasoning_content: Some("new-thought".to_string()),
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
                reasoning_content: None,
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
