use std::sync::{Arc, Mutex};

use serde_json::json;
use vv_agent::runtime::is_prompt_too_long_error;
use vv_agent::{
    build_default_registry, BeforeLlmEvent, BeforeLlmPatch, CycleRunRequest, CycleRunner,
    LLMResponse, LlmError, MemoryManager, MemoryManagerConfig, MemoryWorkspaceBackend, Message,
    MicrocompactionPolicy, RuntimeHook, RuntimeHookManager, ScriptStep, ScriptedLlmClient,
    ToolCall, MAX_PROMPT_TOO_LONG_RETRIES,
};

struct RemoveReadFileHook;

impl RuntimeHook for RemoveReadFileHook {
    fn before_llm(&self, event: BeforeLlmEvent<'_>) -> Option<BeforeLlmPatch> {
        Some(BeforeLlmPatch {
            messages: None,
            tool_schemas: Some(
                event
                    .tool_schemas
                    .iter()
                    .filter(|schema| schema["function"]["name"] != "read_file")
                    .cloned()
                    .collect(),
            ),
        })
    }
}

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
    let task = vv_agent::types::AgentTask::new(
        "cycle_api",
        "demo",
        vv_agent::prompt::PromptBundle::from_instruction_text("system").expect("prompt bundle"),
        "prompt",
    );
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
    let task = vv_agent::types::AgentTask::new(
        "cycle_microcompact",
        "demo",
        vv_agent::prompt::PromptBundle::from_instruction_text("system").expect("prompt bundle"),
        "prompt",
    );
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
        microcompaction_policy: MicrocompactionPolicy::new(0.2, 0.1, 0, 200).expect("policy"),
        ..MemoryManagerConfig::default()
    })
    .with_workspace_backend(Arc::new(MemoryWorkspaceBackend::default()))
    .with_recovery_tool_available(true);

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
                .with_previous_prompt_tokens(Some(750)),
        )
        .expect("cycle");

    assert!(cycle.memory_compacted);
    let captured = captured_requests.lock().expect("captured");
    let request_messages = captured.first().expect("llm request");
    assert!(
        request_messages.iter().any(|message| message
            .content
            .starts_with(vv_agent::memory::TOOL_RESULT_COMPACT_MARKER)),
        "previous prompt token pressure should first archive old tool output: {request_messages:#?}"
    );
    assert!(
        request_messages
            .iter()
            .all(|message| !message.content.contains("<Compressed Agent Memory>")),
        "microcompact should avoid full summary when the reduced request fits: {request_messages:#?}"
    );
}

#[test]
fn cycle_runner_rejects_compacted_results_when_hook_removes_read_file() {
    let runner = CycleRunner::new(
        ScriptedLlmClient::from_steps(vec![ScriptStep::callback(|_| {
            panic!("LLM must not be called without a recovery tool")
        })]),
        build_default_registry(),
    )
    .with_hook_manager(RuntimeHookManager::new(vec![Arc::new(RemoveReadFileHook)]));
    let task = vv_agent::types::AgentTask::new(
        "cycle_missing_recovery",
        "demo",
        vv_agent::prompt::PromptBundle::from_instruction_text("system").expect("prompt bundle"),
        "prompt",
    );
    let mut memory_manager = MemoryManager::new(MemoryManagerConfig::default())
        .with_workspace_backend(Arc::new(MemoryWorkspaceBackend::default()))
        .with_recovery_tool_available(true);
    let messages = vec![
        Message::system("system"),
        Message::user("prompt"),
        Message::tool(
            format!(
                "{}\ntool_name: custom_search\nartifact_path: .vv-agent/artifacts/run/call.txt\n\
                 retrieval_hint: use read_file on artifact_path if needed\nexcerpt:\nprior\n\
                 </Tool Result Compact>",
                vv_agent::memory::TOOL_RESULT_COMPACT_MARKER
            ),
            "call",
        ),
    ];

    let error = runner
        .run_cycle(CycleRunRequest::new(
            &task,
            messages,
            4,
            &mut memory_manager,
        ))
        .expect_err("missing recovery tool must fail closed");

    assert!(error
        .to_string()
        .contains("microcompaction_recovery_unavailable"));
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
    let task = vv_agent::types::AgentTask::new(
        "cycle_prompt_retry",
        "demo",
        vv_agent::prompt::PromptBundle::from_instruction_text("system").expect("prompt bundle"),
        "prompt",
    );
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
    let task = vv_agent::types::AgentTask::new(
        "cycle_prompt_second_retry",
        "demo",
        vv_agent::prompt::PromptBundle::from_instruction_text("system").expect("prompt bundle"),
        "prompt",
    );
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
    let task = vv_agent::types::AgentTask::new(
        "cycle_prompt_exhausted",
        "demo",
        vv_agent::prompt::PromptBundle::from_instruction_text("system").expect("prompt bundle"),
        "prompt",
    );
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
    let task = vv_agent::types::AgentTask::new(
        "cycle_prompt_other_error",
        "demo",
        vv_agent::prompt::PromptBundle::from_instruction_text("system").expect("prompt bundle"),
        "prompt",
    );
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
