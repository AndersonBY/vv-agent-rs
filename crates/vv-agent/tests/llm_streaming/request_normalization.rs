use super::*;

#[test]
fn vv_llm_client_converts_extra_minimax_system_messages() {
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
fn vv_llm_client_omits_empty_optional_request_fields() {
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
        vec![vv_llm::MessageContent::text("inspect")]
    );
}

#[test]
fn vv_llm_client_preserves_reasoning_and_tool_extra_content_through_vv_llm() {
    let chat_client = RecordingMessagesChatClient::default();
    let probe = chat_client.clone();
    let llm = VvLlmClient::new(
        "deepseek",
        "deepseek-v5-pro",
        "deepseek-v5-pro",
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
fn vv_llm_client_preserves_reasoning_chain_for_deepseek_tool_turns() {
    let chat_client = RecordingMessagesChatClient::default();
    let probe = chat_client.clone();
    let llm = VvLlmClient::new(
        "deepseek",
        "deepseek-v4-pro",
        "deepseek-v4-pro",
        Box::new(chat_client),
        90.0,
    );
    let assistant_with_reasoning = {
        let mut message = Message::assistant("first");
        message.reasoning_content = Some("old-thought".to_string());
        message
    };
    let assistant_without_reasoning = Message::assistant("second");

    let _ = llm
        .complete(LlmRequest::new(
            "deepseek-v5-pro",
            vec![
                Message::user("start"),
                assistant_with_reasoning,
                Message::user("continue"),
                assistant_without_reasoning,
            ],
        ))
        .expect("deepseek request");

    let request = probe.last_request().expect("recorded request");
    let assistant_messages = request
        .messages
        .iter()
        .filter(|message| message.role == vv_llm::MessageRole::Assistant)
        .collect::<Vec<_>>();
    assert_eq!(assistant_messages.len(), 2);
    assert_eq!(
        assistant_messages[0].reasoning_content.as_deref(),
        Some("old-thought")
    );
    assert_eq!(assistant_messages[1].reasoning_content.as_deref(), Some(""));
}

#[test]
fn vv_llm_client_applies_deepseek_reasoning_profile() {
    let chat_client = RecordingMessagesChatClient::default();
    let probe = chat_client.clone();
    let llm = VvLlmClient::new(
        "deepseek",
        "deepseek-v5-pro",
        "deepseek-v5-pro",
        Box::new(chat_client),
        90.0,
    );

    let _ = llm
        .complete(LlmRequest::new(
            "deepseek-v5-pro",
            vec![Message::user("use reasoning profile")],
        ))
        .expect("deepseek request");

    let request = probe.last_request().expect("recorded request");
    assert_eq!(request.options.temperature, None);
    assert_eq!(request.extra_body["thinking"], json!({"type": "enabled"}));
    assert_eq!(request.extra_body["reasoning_effort"], json!("max"));
}

#[test]
fn vv_llm_client_normalizes_supported_thinking_model_options() {
    let claude_client = RecordingMessagesChatClient::default();
    let claude_probe = claude_client.clone();
    let claude = VvLlmClient::new(
        "anthropic",
        "claude-opus-4-7-thinking",
        "claude-opus-4-7-thinking",
        Box::new(claude_client),
        90.0,
    );
    let _ = claude
        .complete(LlmRequest::new(
            "claude-opus-4-7-thinking",
            vec![Message::user("think")],
        ))
        .expect("claude thinking request");

    let claude_request = claude_probe.last_request().expect("claude request");
    assert_eq!(claude_request.model, "claude-opus-4-7");
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
fn vv_llm_client_applies_claude_prompt_cache_through_vv_llm_types() {
    let chat_client = RecordingMessagesChatClient::default();
    let probe = chat_client.clone();
    let llm = VvLlmClient::new(
        "anthropic",
        "claude-sonnet-4-6",
        "claude-sonnet-4-6",
        Box::new(chat_client),
        90.0,
    );
    let mut request = LlmRequest::new(
        "claude-sonnet-4-6",
        vec![
            Message::system("fallback system text"),
            Message::user("latest user turn ".repeat(350)),
        ],
    );
    request.metadata = json!({
        PROMPT_CACHE_ENABLED_KEY: true,
        SYSTEM_PROMPT_SECTIONS_KEY: [
            {"id": "stable", "text": "stable section ".repeat(400), "stable": true}
        ]
    });
    request.tools = vec![json!({
        "type": "function",
        "function": {
            "name": "default_api:read_file",
            "description": "Read a file.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                }
            }
        }
    })];

    let _ = llm.complete(request).expect("claude cached request");

    let request = probe.last_request().expect("recorded request");
    let system_message = request
        .messages
        .iter()
        .find(|message| message.role == vv_llm::MessageRole::System)
        .expect("system message");
    assert!(matches!(
        system_message.content.last(),
        Some(vv_llm::MessageContent::Text {
            cache_control: Some(value),
            ..
        }) if value == &json!({"type": "ephemeral"})
    ));
    assert_eq!(
        request
            .tools
            .last()
            .and_then(|tool| tool.cache_control.as_ref()),
        Some(&json!({"type": "ephemeral"}))
    );
    let user_message = request
        .messages
        .iter()
        .rev()
        .find(|message| message.role == vv_llm::MessageRole::User)
        .expect("history user message");
    assert!(matches!(
        user_message.content.last(),
        Some(vv_llm::MessageContent::Text {
            cache_control: Some(value),
            ..
        }) if value == &json!({"type": "ephemeral"})
    ));
}

#[test]
fn vv_llm_client_normalizes_more_provider_model_aliases() {
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
fn vv_llm_client_normalizes_tool_call_ids_and_names() {
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
fn vv_llm_stream_collects_raw_content_blocks() {
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
fn vv_llm_client_debug_dump_writes_request_messages() {
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
