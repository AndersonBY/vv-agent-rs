use std::sync::{Arc, Mutex};

use serde_json::json;
use vv_agent::runtime::is_prompt_too_long_error;
use vv_agent::{
    build_default_registry, CycleRunRequest, CycleRunner, LLMResponse, LlmError, MemoryManager,
    MemoryManagerConfig, Message, ScriptStep, ScriptedLlmClient, ToolCall,
    MAX_PROMPT_TOO_LONG_RETRIES,
};

#[test]
fn cycle_runner_public_api_builds_assistant_message() {
    let mut response = LLMResponse::new("cycle done");
    response
        .raw
        .insert("reasoning_content".to_string(), json!("cycle reasoning"));
    let runner = CycleRunner::new(
        ScriptedLlmClient::new(vec![response]),
        build_default_registry(),
    );
    let task = vv_agent::AgentTask::new("cycle_api", "demo", "system", "prompt");
    let mut memory_manager = MemoryManager::new(MemoryManagerConfig::default());

    let (messages, cycle) = runner
        .run_cycle(CycleRunRequest::new(
            &task,
            vec![Message::system("system"), Message::user("prompt")],
            1,
            &mut memory_manager,
        ))
        .expect("cycle");

    assert_eq!(cycle.index, 1);
    assert_eq!(cycle.assistant_message, "cycle done");
    assert_eq!(messages.last().expect("assistant").content, "cycle done");
    assert_eq!(
        messages
            .last()
            .expect("assistant")
            .reasoning_content
            .as_deref(),
        Some("cycle reasoning")
    );
}

#[test]
fn cycle_runner_microcompacts_before_full_compaction_when_previous_prompt_tokens_are_high() {
    let captured_requests = Arc::new(Mutex::new(Vec::<Vec<Message>>::new()));
    let captured_for_step = Arc::clone(&captured_requests);
    let runner = CycleRunner::new(
        ScriptedLlmClient::from_steps(vec![ScriptStep::callback(move |request| {
            captured_for_step
                .lock()
                .expect("capture")
                .push(request.messages.clone());
            Ok(LLMResponse::new("done"))
        })]),
        build_default_registry(),
    );
    let task = vv_agent::AgentTask::new("cycle_microcompact", "demo", "system", "prompt");
    let mut memory_manager = MemoryManager::new(MemoryManagerConfig {
        model: "demo".to_string(),
        model_context_window: 800,
        reserved_output_tokens: 50,
        autocompact_buffer_tokens: 50,
        summary_callback: Some(Arc::new(|_, _, _| {
            Some(
                json!({
                    "summary_version": 1,
                    "progress": ["summarized"],
                    "key_facts": [],
                    "open_issues": [],
                    "next_steps": []
                })
                .to_string(),
            )
        })),
        tool_result_compact_threshold: 10_000,
        microcompact_trigger_ratio: 0.2,
        microcompact_keep_recent_cycles: 0,
        microcompact_min_result_length: 200,
        ..MemoryManagerConfig::default()
    });

    let mut assistant = Message::assistant("old tool call");
    assistant
        .tool_calls
        .push(ToolCall::new("call_old", "read_file", Default::default()));
    let messages = vec![
        Message::system("system"),
        Message::user("original request"),
        assistant,
        Message::tool("x".repeat(2_000), "call_old"),
        Message::user("latest request"),
    ];

    let (_messages, cycle) = runner
        .run_cycle(
            CycleRunRequest::new(&task, messages, 3, &mut memory_manager)
                .with_previous_prompt_tokens(Some(5_000)),
        )
        .expect("cycle");

    assert!(cycle.memory_compacted);
    let captured = captured_requests.lock().expect("captured");
    let request_messages = captured.first().expect("llm request");
    assert!(
        request_messages.iter().any(|message| {
            message.content == vv_agent::memory::CLEARED_MARKER
                && message
                    .metadata
                    .get("microcompacted")
                    .is_some_and(|value| value == true)
        }),
        "previous prompt token pressure should first clear old compactable tool output: {request_messages:#?}"
    );
    assert!(
        request_messages
            .iter()
            .all(|message| !message.content.contains("<Compressed Agent Memory>")),
        "microcompact should avoid full summary when the reduced request fits: {request_messages:#?}"
    );
}

#[test]
fn cycle_runner_retries_prompt_too_long_with_forced_compaction() {
    let captured_requests = Arc::new(Mutex::new(Vec::<Vec<Message>>::new()));
    let captured_for_step = Arc::clone(&captured_requests);
    let runner = CycleRunner::new(
        ScriptedLlmClient::from_steps(vec![
            prompt_too_long_step("Prompt is too long for this model"),
            ScriptStep::callback(move |request| {
                captured_for_step
                    .lock()
                    .expect("capture")
                    .push(request.messages.clone());
                Ok(LLMResponse::new("done"))
            }),
        ]),
        build_default_registry(),
    );
    let task = vv_agent::AgentTask::new("cycle_prompt_retry", "demo", "system", "prompt");
    let mut memory_manager = prompt_retry_memory_manager();

    let (messages, cycle) = runner
        .run_cycle(CycleRunRequest::new(
            &task,
            retry_fixture_messages(),
            1,
            &mut memory_manager,
        ))
        .expect("cycle");

    assert!(cycle.memory_compacted);
    assert_eq!(messages.last().expect("assistant").content, "done");
    let captured = captured_requests.lock().expect("captured");
    let retry_request = captured.first().expect("retry request");
    assert!(
        retry_request
            .iter()
            .any(|message| message.content.contains("<Compressed Agent Memory>")),
        "retry request should include compressed memory: {retry_request:#?}"
    );
}

#[test]
fn cycle_runner_retries_prompt_too_long_then_accepts_second_retry() {
    let captured_requests = Arc::new(Mutex::new(Vec::<Vec<Message>>::new()));
    let captured_for_step = Arc::clone(&captured_requests);
    let runner = CycleRunner::new(
        ScriptedLlmClient::from_steps(vec![
            prompt_too_long_step("Prompt is too long for this model"),
            prompt_too_long_step("context_length_exceeded"),
            ScriptStep::callback(move |request| {
                captured_for_step
                    .lock()
                    .expect("capture")
                    .push(request.messages.clone());
                Ok(LLMResponse::new("done after retry"))
            }),
        ]),
        build_default_registry(),
    );
    let task = vv_agent::AgentTask::new("cycle_prompt_second_retry", "demo", "system", "prompt");
    let mut memory_manager = prompt_retry_memory_manager();

    let (messages, cycle) = runner
        .run_cycle(CycleRunRequest::new(
            &task,
            retry_fixture_messages(),
            1,
            &mut memory_manager,
        ))
        .expect("cycle");

    assert!(cycle.memory_compacted);
    assert_eq!(
        messages.last().expect("assistant").content,
        "done after retry"
    );
    let captured = captured_requests.lock().expect("captured");
    let final_request = captured.first().expect("final request");
    assert!(
        final_request.len() <= 2,
        "second retry should use compacted messages: {final_request:#?}"
    );
}

#[test]
fn cycle_runner_returns_compaction_exhausted_after_prompt_too_long_retries() {
    let steps = (0..=MAX_PROMPT_TOO_LONG_RETRIES)
        .map(|_| prompt_too_long_step("request too large"))
        .collect::<Vec<_>>();
    let runner = CycleRunner::new(
        ScriptedLlmClient::from_steps(steps),
        build_default_registry(),
    );
    let task = vv_agent::AgentTask::new("cycle_prompt_exhausted", "demo", "system", "prompt");
    let mut memory_manager = prompt_retry_memory_manager();

    let error = runner
        .run_cycle(CycleRunRequest::new(
            &task,
            retry_fixture_messages(),
            1,
            &mut memory_manager,
        ))
        .expect_err("compaction exhausted error");

    match error {
        LlmError::CompactionExhausted(error) => {
            assert_eq!(error.attempts, MAX_PROMPT_TOO_LONG_RETRIES + 1);
            assert!(error
                .last_error
                .as_deref()
                .is_some_and(|message| message.contains("request too large")));
        }
        other => panic!("expected compaction exhausted error, got {other:?}"),
    }
}

#[test]
fn cycle_runner_propagates_non_prompt_too_long_errors() {
    let runner = CycleRunner::new(
        ScriptedLlmClient::from_steps(vec![ScriptStep::callback(|_| {
            Err(LlmError::Request("network down".to_string()))
        })]),
        build_default_registry(),
    );
    let task = vv_agent::AgentTask::new("cycle_prompt_other_error", "demo", "system", "prompt");
    let mut memory_manager = prompt_retry_memory_manager();

    let error = runner
        .run_cycle(CycleRunRequest::new(
            &task,
            vec![Message::system("system"), Message::user("hello")],
            1,
            &mut memory_manager,
        ))
        .expect_err("network error");

    assert!(error.to_string().contains("network down"));
}

#[test]
fn cycle_runner_recognizes_prompt_too_long_error_patterns() {
    assert!(is_prompt_too_long_error(&LlmError::Request(
        "maximum context length exceeded".to_string()
    )));
    assert!(is_prompt_too_long_error(&LlmError::Request(
        "request too large".to_string()
    )));
    assert!(!is_prompt_too_long_error(&LlmError::Request(
        "network down".to_string()
    )));
}

fn prompt_too_long_step(message: &'static str) -> ScriptStep {
    ScriptStep::callback(move |_| Err(LlmError::Request(message.to_string())))
}

fn prompt_retry_memory_manager() -> MemoryManager {
    MemoryManager::new(MemoryManagerConfig {
        model: "demo".to_string(),
        model_context_window: 60,
        reserved_output_tokens: 10,
        autocompact_buffer_tokens: 10,
        summary_callback: Some(Arc::new(|_, _, _| {
            Some(
                json!({
                    "summary_version": 1,
                    "progress": ["done"],
                    "key_facts": [],
                    "open_issues": [],
                    "next_steps": []
                })
                .to_string(),
            )
        })),
        keep_recent_messages: 1,
        ..MemoryManagerConfig::default()
    })
}

fn retry_fixture_messages() -> Vec<Message> {
    vec![
        Message::system("system"),
        Message::user("u".repeat(40)),
        Message::assistant("a".repeat(40)),
        Message::user("c".repeat(40)),
    ]
}
