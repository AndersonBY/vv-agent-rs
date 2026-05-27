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
