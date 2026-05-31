use super::*;

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
fn structured_stream_tool_events_preserve_provider_tool_call_index() {
    let llm = VvLlmClient::new(
        "moonshot",
        "kimi-k2.5",
        "kimi-k2.5",
        Box::new(MultiToolIndexStreamingChatClient),
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
            LlmRequest::new("kimi-k2.5", vec![Message::user("stream tool index")]),
            Some(stream_callback),
        )
        .expect("multi-tool index streaming completion");

    assert_eq!(response.tool_calls[0].id, "call_2");
    let events = events.lock().expect("stream events lock");
    assert_eq!(
        events
            .iter()
            .filter(|event| event.get("event") == Some(&json!("tool_call_started")))
            .map(|event| event["tool_call_index"].clone())
            .collect::<Vec<_>>(),
        vec![json!(2)]
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.get("event") == Some(&json!("tool_call_progress")))
            .map(|event| event["tool_call_index"].clone())
            .collect::<Vec<_>>(),
        vec![json!(2), json!(2)]
    );
}

#[test]
fn structured_stream_events_estimate_tokens_from_char_count() {
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
fn runtime_controls_stream_callback_is_forwarded_to_runtime() {
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
    let task = AgentTask::new(
        "stream-callback",
        "deepseek-v4-pro",
        "Use task_finish when finished.",
        "finish via runtime stream",
    );
    let result = runtime
        .run_with_controls(
            task,
            RuntimeRunControls {
                execution_context: Some(
                    ExecutionContext::default().with_stream_callback(stream_callback),
                ),
                ..RuntimeRunControls::default()
            },
        )
        .expect("runtime run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(result.final_answer.as_deref(), Some("streamed answer"));
    assert_eq!(probe.completion_calls(), 0);
    assert_eq!(probe.stream_calls(), 1);
    assert!(!events.lock().expect("stream events lock").is_empty());
}

#[test]
fn vv_llm_client_estimates_usage_when_provider_omits_usage() {
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
fn vv_llm_client_auto_streams_deepseek_v4_models() {
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
