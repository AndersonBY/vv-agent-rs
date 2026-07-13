use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::stream;
use serde_json::{json, Value};
use vv_agent::llm::{PROMPT_CACHE_ENABLED_KEY, SYSTEM_PROMPT_SECTIONS_KEY};
use vv_agent::{
    AgentRuntime, AgentStatus, AgentTask, ExecutionContext, LlmClient, LlmRequest, Message,
    ModelSettings, ResponseFormat, RuntimeRunControls, StreamCallback, ToolChoice, VvLlmClient,
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

#[derive(Clone, Default)]
struct MultiToolIndexStreamingChatClient;

impl vv_llm::ChatClient for MultiToolIndexStreamingChatClient {
    fn provider_name(&self) -> &'static str {
        "test-multi-tool-index-streaming"
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
                id: "multi-tool-non-stream-response".to_string(),
                model: "kimi-k2.5".to_string(),
                content: String::new(),
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
            let mut first = vv_llm::ToolCall::function("call_2", "todo_write", "{\"todos\":[");
            first.index = Some(2);
            let mut second = vv_llm::ToolCall::function("", "", "{\"title\":\"a\"}");
            second.index = Some(2);
            let deltas = vec![
                Ok(vv_llm::ChatStreamDelta {
                    tool_calls: vec![first],
                    ..vv_llm::ChatStreamDelta::default()
                }),
                Ok(vv_llm::ChatStreamDelta {
                    tool_calls: vec![second],
                    done: true,
                    ..vv_llm::ChatStreamDelta::default()
                }),
            ];
            Ok(Box::pin(stream::iter(deltas)) as vv_llm::ChatStream)
        })
    }
}

#[path = "llm_streaming/failover.rs"]
mod failover;
#[path = "llm_streaming/request_normalization.rs"]
mod request_normalization;
#[path = "llm_streaming/streaming.rs"]
mod streaming;

#[derive(Clone, Default)]
struct CountingFailingChatClient {
    calls: Arc<Mutex<u32>>,
}

#[derive(Clone, Default)]
struct ConfigurationFailingChatClient;

impl vv_llm::ChatClient for ConfigurationFailingChatClient {
    fn provider_name(&self) -> &'static str {
        "configuration-failing"
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
            Err(vv_llm::VvLlmError::Configuration(
                "invalid local configuration".to_string(),
            ))
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
            Err(vv_llm::VvLlmError::Configuration(
                "invalid local configuration".to_string(),
            ))
        })
    }
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
                return Err(vv_llm::VvLlmError::Http(
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
        let failures_remaining = Arc::clone(&self.failures_remaining);
        let calls = Arc::clone(&self.calls);
        Box::pin(async move {
            *calls.lock().expect("flaky calls lock") += 1;
            let mut failures = failures_remaining.lock().expect("flaky failures lock");
            if *failures > 0 {
                *failures -= 1;
                return Err(vv_llm::VvLlmError::Http(
                    "transient endpoint error".to_string(),
                ));
            }
            let chat_stream: vv_llm::ChatStream =
                Box::pin(stream::iter([Ok(vv_llm::ChatStreamDelta {
                    content: "flaky success".to_string(),
                    done: true,
                    ..vv_llm::ChatStreamDelta::default()
                })]));
            assert_eq!(request.options.stream, Some(true));
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
            let chat_stream: vv_llm::ChatStream =
                Box::pin(stream::iter([Ok(vv_llm::ChatStreamDelta {
                    tool_calls: vec![vv_llm::ToolCall::function(
                        "",
                        "task _finish",
                        r#"{"message":"done"}"#,
                    )],
                    done: true,
                    ..vv_llm::ChatStreamDelta::default()
                })]));
            Ok(chat_stream)
        })
    }
}

#[derive(Clone, Default)]
struct RecordingMessagesChatClient {
    requests: Arc<Mutex<Vec<vv_llm::ChatRequest>>>,
}

#[derive(Clone)]
struct DelayedChatClient {
    delay: Duration,
}

impl vv_llm::ChatClient for DelayedChatClient {
    fn provider_name(&self) -> &'static str {
        "delayed"
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
        let delay = self.delay;
        Box::pin(async move {
            tokio::time::sleep(delay).await;
            Ok(vv_llm::ChatResponse {
                id: "delayed-response".to_string(),
                model: request.model,
                content: "done".to_string(),
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
        let delay = self.delay;
        Box::pin(async move {
            tokio::time::sleep(delay).await;
            Ok(Box::pin(stream::iter([Ok(vv_llm::ChatStreamDelta {
                content: "done".to_string(),
                done: true,
                ..vv_llm::ChatStreamDelta::default()
            })])) as vv_llm::ChatStream)
        })
    }
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
                    index: None,
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
            let mut tool_call = vv_llm::ToolCall {
                id: "call_response".to_string(),
                name: "default_api:read_file".to_string(),
                arguments: r#"{"path":"README.md"}"#.to_string(),
                index: None,
                extra_content: Some(json!({"google": {"thought_signature": "sig_456"}})),
            };
            tool_call.index = Some(0);
            let chat_stream: vv_llm::ChatStream =
                Box::pin(stream::iter([Ok(vv_llm::ChatStreamDelta {
                    content: "recorded".to_string(),
                    reasoning_content: "new-thought".to_string(),
                    tool_calls: vec![tool_call],
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
            let chat_stream: vv_llm::ChatStream =
                Box::pin(stream::iter([Ok(vv_llm::ChatStreamDelta {
                    content: "estimated usage response".to_string(),
                    done: true,
                    ..vv_llm::ChatStreamDelta::default()
                })]));
            Ok(chat_stream)
        })
    }
}
