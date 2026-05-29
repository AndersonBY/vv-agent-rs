use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::sync::{Arc, Mutex};

use serde_json::json;
use vv_agent::{
    memory::CLEARED_MARKER, AgentRuntime, AgentStatus, AgentTask, BeforeLlmPatch,
    BeforeToolCallPatch, CancellationToken, ExecutionContext, LLMResponse, LlmClient, LlmError,
    LlmRequest, LlmStreamCallback, Message, RuntimeHook, RuntimeRunControls, ScriptedLlmClient,
    SubAgentConfig, TokenUsage, ToolCall, ToolDirective, ToolExecutionResult,
};

const PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x60, 0x00, 0x00, 0x00,
    0x02, 0x00, 0x01, 0xe2, 0x21, 0xbc, 0x33, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];

fn preview_text_for_test(text: &str, log_preview_chars: Option<usize>) -> String {
    let cleaned = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let Some(limit) = log_preview_chars.map(|limit| limit.max(40)) else {
        return cleaned;
    };
    if cleaned.chars().count() <= limit {
        return cleaned;
    }
    format!(
        "{}...",
        cleaned
            .chars()
            .take(limit.saturating_sub(3))
            .collect::<String>()
    )
}

#[test]
fn runtime_executes_tool_calls_until_task_finish() {
    let mut finish_args = BTreeMap::new();
    finish_args.insert(
        "message".to_string(),
        json!("final answer from task_finish"),
    );
    let llm = ScriptedLlmClient::new(vec![LLMResponse::with_tool_calls(
        "",
        vec![ToolCall::new("call_1", "task_finish", finish_args)],
    )]);
    let runtime = AgentRuntime::new(llm);

    let result = runtime
        .run(AgentTask::new("task_1", "demo", "system", "finish now"))
        .expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(
        result.final_answer.as_deref(),
        Some("final answer from task_finish")
    );
    assert_eq!(result.cycles.len(), 1);
    assert_eq!(result.cycles[0].tool_results.len(), 1);
    assert_eq!(
        result.cycles[0].tool_results[0].directive,
        ToolDirective::Finish
    );
    assert_eq!(
        result.messages.last().unwrap().tool_call_id.as_deref(),
        Some("call_1")
    );
}

#[test]
fn runtime_preserves_reasoning_content_on_assistant_messages() {
    let mut response = LLMResponse::new("plain answer");
    response
        .raw
        .insert("reasoning_content".to_string(), json!("private analysis"));
    let runtime = AgentRuntime::new(ScriptedLlmClient::new(vec![response]));
    let mut task = AgentTask::new("reasoning_task", "demo", "system", "prompt");
    task.no_tool_policy = vv_agent::NoToolPolicy::Finish;

    let result = runtime.run(task).expect("run");

    let assistant = result
        .messages
        .iter()
        .find(|message| message.role == vv_agent::MessageRole::Assistant)
        .expect("assistant message");
    assert_eq!(
        assistant.reasoning_content.as_deref(),
        Some("private analysis")
    );
}

#[test]
fn runtime_preserves_shared_state_when_task_finishes() {
    let todo_args = BTreeMap::from([(
        "todos".to_string(),
        json!([
            {"id": "t1", "title": "done item", "status": "completed"}
        ]),
    )]);
    let finish_args = BTreeMap::from([("message".to_string(), json!("done"))]);
    let llm = ScriptedLlmClient::new(vec![LLMResponse::with_tool_calls(
        "",
        vec![
            ToolCall::new("todo_call", "todo_write", todo_args),
            ToolCall::new("finish_call", "task_finish", finish_args),
        ],
    )]);
    let runtime = AgentRuntime::new(llm);

    let result = runtime
        .run(AgentTask::new("task_state", "demo", "system", "finish"))
        .expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(result.todo_list()[0]["title"], "done item");
}

#[test]
fn runtime_waits_when_ask_user_tool_requests_input() {
    let mut ask_args = BTreeMap::new();
    ask_args.insert("question".to_string(), json!("Which option should I use?"));
    let llm = ScriptedLlmClient::new(vec![LLMResponse::with_tool_calls(
        "",
        vec![ToolCall::new("call_1", "ask_user", ask_args)],
    )]);
    let runtime = AgentRuntime::new(llm);

    let result = runtime
        .run(AgentTask::new("task_1", "demo", "system", "ask"))
        .expect("run");

    assert_eq!(result.status, AgentStatus::WaitUser);
    assert_eq!(
        result.wait_reason.as_deref(),
        Some("Which option should I use?")
    );
}

#[test]
fn runtime_marks_remaining_tool_calls_skipped_after_wait_user() {
    let ask_args = BTreeMap::from([("question".to_string(), json!("First question?"))]);
    let second_args = BTreeMap::from([("question".to_string(), json!("Second question?"))]);
    let llm = ScriptedLlmClient::new(vec![LLMResponse::with_tool_calls(
        "",
        vec![
            ToolCall::new("call_1", "ask_user", ask_args),
            ToolCall::new("call_2", "ask_user", second_args),
        ],
    )]);
    let runtime = AgentRuntime::new(llm);

    let result = runtime
        .run(AgentTask::new("task_skip_wait", "demo", "system", "ask"))
        .expect("run");

    assert_eq!(result.status, AgentStatus::WaitUser);
    assert_eq!(result.cycles[0].tool_results.len(), 2);
    assert_eq!(result.cycles[0].tool_results[1].tool_call_id, "call_2");
    assert_eq!(
        result.cycles[0].tool_results[1].error_code.as_deref(),
        Some("skipped_due_to_wait_user")
    );
}

#[test]
fn runtime_injects_image_message_after_read_image() {
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::write(workspace.path().join("img.png"), PNG_1X1).expect("image");

    let mut read_image_args = BTreeMap::new();
    read_image_args.insert("path".to_string(), json!("img.png"));
    let mut finish_args = BTreeMap::new();
    finish_args.insert("message".to_string(), json!("ok"));

    let llm = InspectingImageLlmClient::new(
        LLMResponse::with_tool_calls(
            "read image",
            vec![ToolCall::new("call_1", "read_image", read_image_args)],
        ),
        LLMResponse::with_tool_calls(
            "done",
            vec![ToolCall::new("call_2", "task_finish", finish_args)],
        ),
    );
    let inspector = llm.clone();
    let mut runtime = AgentRuntime::new(llm);
    runtime.default_workspace = Some(workspace.path().to_path_buf());
    runtime.workspace_backend = std::sync::Arc::new(
        vv_agent::workspace::LocalWorkspaceBackend::new(workspace.path()),
    );

    let mut task = AgentTask::new("task_img", "demo", "system", "read image");
    task.native_multimodal = true;
    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert!(inspector.saw_image_message());
}

#[test]
fn runtime_does_not_inject_image_message_for_text_only_task() {
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::write(workspace.path().join("img.png"), PNG_1X1).expect("image");

    let mut read_image_args = BTreeMap::new();
    read_image_args.insert("path".to_string(), json!("img.png"));
    let mut finish_args = BTreeMap::new();
    finish_args.insert("message".to_string(), json!("ok"));

    let llm = InspectingImageLlmClient::new(
        LLMResponse::with_tool_calls(
            "read image",
            vec![ToolCall::new("call_1", "read_image", read_image_args)],
        ),
        LLMResponse::with_tool_calls(
            "done",
            vec![ToolCall::new("call_2", "task_finish", finish_args)],
        ),
    );
    let inspector = llm.clone();
    let mut runtime = AgentRuntime::new(llm);
    runtime.default_workspace = Some(workspace.path().to_path_buf());
    runtime.workspace_backend = std::sync::Arc::new(
        vv_agent::workspace::LocalWorkspaceBackend::new(workspace.path()),
    );

    let task = AgentTask::new("task_img_text_only", "demo", "system", "read image");
    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert!(!inspector.saw_image_message());
}

#[test]
fn runtime_keeps_tool_results_adjacent_before_image_notifications() {
    let mut registry = vv_agent::tools::build_default_registry();
    registry
        .register_tool(
            "_demo_image",
            "demo image",
            Arc::new(|_context, _arguments| {
                let mut result = ToolExecutionResult::success("", r#"{"ok":true}"#);
                result.image_url = Some("data:image/png;base64,AAAA".to_string());
                result
            }),
        )
        .expect("register image tool");

    let todo_args = BTreeMap::from([(
        "todos".to_string(),
        json!([{"title": "done", "status": "completed", "priority": "medium"}]),
    )]);
    let finish_args = BTreeMap::from([("message".to_string(), json!("ok"))]);
    let llm = MessageOrderInspectingLlmClient::new(
        LLMResponse::with_tool_calls(
            "run tools",
            vec![
                ToolCall::new("img1", "_demo_image", BTreeMap::new()),
                ToolCall::new("todo1", "todo_write", todo_args),
            ],
        ),
        LLMResponse::with_tool_calls(
            "done",
            vec![ToolCall::new("finish_order", "task_finish", finish_args)],
        ),
    );
    let inspector = llm.clone();
    let runtime = AgentRuntime::new(llm).with_tool_registry(registry);
    let mut task = AgentTask::new("task_image_order", "demo", "system", "go");
    task.max_cycles = 4;
    task.native_multimodal = true;
    task.extra_tool_names = vec!["_demo_image".to_string()];

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    let second_request = inspector.second_request_messages();
    let assistant_index = second_request
        .iter()
        .position(|message| {
            message.role == vv_agent::MessageRole::Assistant && !message.tool_calls.is_empty()
        })
        .expect("assistant tool call message");
    assert_eq!(
        second_request[assistant_index + 1].tool_call_id.as_deref(),
        Some("img1")
    );
    assert_eq!(
        second_request[assistant_index + 2].tool_call_id.as_deref(),
        Some("todo1")
    );
    let image_message = &second_request[assistant_index + 3];
    assert_eq!(image_message.role, vv_agent::MessageRole::User);
    assert_eq!(image_message.content, "");
    assert_eq!(
        image_message.image_url.as_deref(),
        Some("data:image/png;base64,AAAA")
    );
}

#[test]
fn runtime_skips_custom_image_notifications_when_multimodal_disabled() {
    let mut registry = vv_agent::tools::build_default_registry();
    registry
        .register_tool(
            "_demo_image",
            "demo image",
            Arc::new(|_context, _arguments| {
                let mut result = ToolExecutionResult::success("", r#"{"ok":true}"#);
                result.image_url = Some("data:image/png;base64,AAAA".to_string());
                result
            }),
        )
        .expect("register image tool");

    let finish_args = BTreeMap::from([("message".to_string(), json!("ok"))]);
    let llm = MessageOrderInspectingLlmClient::new(
        LLMResponse::with_tool_calls(
            "capture",
            vec![ToolCall::new("img1", "_demo_image", BTreeMap::new())],
        ),
        LLMResponse::with_tool_calls(
            "done",
            vec![ToolCall::new("finish_no_image", "task_finish", finish_args)],
        ),
    );
    let inspector = llm.clone();
    let runtime = AgentRuntime::new(llm).with_tool_registry(registry);
    let mut task = AgentTask::new("task_no_multimodal", "demo", "system", "go");
    task.max_cycles = 4;
    task.extra_tool_names = vec!["_demo_image".to_string()];

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(result.final_answer.as_deref(), Some("ok"));
    let second_request = inspector.second_request_messages();
    assert!(!second_request
        .iter()
        .any(|message| message.role == vv_agent::MessageRole::User && message.image_url.is_some()));
}

#[test]
fn runtime_tool_context_uses_execution_context_metadata() {
    let workspace = tempfile::tempdir().expect("workspace");
    let outside = tempfile::tempdir().expect("outside");
    let outside_file = outside.path().join("outside.txt");
    std::fs::write(&outside_file, "outside context metadata").expect("outside file");

    let read_args = BTreeMap::from([("path".to_string(), json!(outside_file))]);
    let finish_args = BTreeMap::from([("message".to_string(), json!("done"))]);
    let llm = ScriptedLlmClient::new(vec![
        LLMResponse::with_tool_calls(
            "read outside file",
            vec![ToolCall::new("read_outside", "read_file", read_args)],
        ),
        LLMResponse::with_tool_calls(
            "finish",
            vec![ToolCall::new("finish", "task_finish", finish_args)],
        ),
    ]);
    let mut runtime = AgentRuntime::new(llm);
    runtime.default_workspace = Some(workspace.path().to_path_buf());
    runtime.workspace_backend = std::sync::Arc::new(
        vv_agent::workspace::LocalWorkspaceBackend::new(workspace.path()),
    );
    let controls = RuntimeRunControls {
        execution_context: Some(ExecutionContext::default().with_metadata(BTreeMap::from([(
            "allow_outside_workspace_paths".to_string(),
            json!(true),
        )]))),
        ..RuntimeRunControls::default()
    };

    let result = runtime
        .run_with_controls(
            AgentTask::new("task_ctx_metadata", "demo", "system", "read"),
            controls,
        )
        .expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(
        result.cycles[0].tool_results[0].status,
        vv_agent::ToolResultStatus::Success
    );
    assert!(result.cycles[0].tool_results[0]
        .content
        .contains("outside context metadata"));
}

#[test]
fn runtime_allows_outside_workspace_paths_from_integer_metadata() {
    let workspace = tempfile::tempdir().expect("workspace");
    let outside = tempfile::tempdir().expect("outside");
    let outside_file = outside.path().join("outside.txt");
    std::fs::write(&outside_file, "outside task metadata").expect("outside file");

    let read_args = BTreeMap::from([("path".to_string(), json!(outside_file))]);
    let finish_args = BTreeMap::from([("message".to_string(), json!("done"))]);
    let llm = ScriptedLlmClient::new(vec![
        LLMResponse::with_tool_calls(
            "read outside file",
            vec![ToolCall::new("read_outside", "read_file", read_args)],
        ),
        LLMResponse::with_tool_calls(
            "finish",
            vec![ToolCall::new("finish", "task_finish", finish_args)],
        ),
    ]);
    let mut runtime = AgentRuntime::new(llm);
    runtime.default_workspace = Some(workspace.path().to_path_buf());
    runtime.workspace_backend = std::sync::Arc::new(
        vv_agent::workspace::LocalWorkspaceBackend::new(workspace.path()),
    );
    let mut task = AgentTask::new("task_metadata_outside", "demo", "system", "read");
    task.metadata
        .insert("allow_outside_workspace_paths".to_string(), json!(1));

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(
        result.cycles[0].tool_results[0].status,
        vv_agent::ToolResultStatus::Success
    );
    assert!(result.cycles[0].tool_results[0]
        .content
        .contains("outside task metadata"));
}

#[test]
fn runtime_executes_configured_sub_agent_with_real_runner() {
    let mut sub_task_args = BTreeMap::new();
    sub_task_args.insert("agent_id".to_string(), json!("researcher"));
    sub_task_args.insert(
        "task_description".to_string(),
        json!("Find the target crate"),
    );
    let mut child_finish_args = BTreeMap::new();
    child_finish_args.insert("message".to_string(), json!("child found vv-llm"));
    let mut parent_finish_args = BTreeMap::new();
    parent_finish_args.insert("message".to_string(), json!("parent saw child result"));

    let llm = ScriptedLlmClient::new(vec![
        LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::new(
                "parent_sub_call",
                "create_sub_task",
                sub_task_args,
            )],
        ),
        LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::new(
                "child_finish",
                "task_finish",
                child_finish_args,
            )],
        ),
        LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::new(
                "parent_finish",
                "task_finish",
                parent_finish_args,
            )],
        ),
    ]);
    let runtime = AgentRuntime::new(llm);
    let mut task = AgentTask::new("parent", "demo", "parent system", "delegate");
    task.sub_agents.insert(
        "researcher".to_string(),
        SubAgentConfig::new("demo", "research profile"),
    );

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(
        result.final_answer.as_deref(),
        Some("parent saw child result")
    );
    let sub_task_result = result
        .cycles
        .iter()
        .flat_map(|cycle| &cycle.tool_results)
        .find(|tool_result| tool_result.tool_call_id == "parent_sub_call")
        .expect("sub-task tool result");
    assert_eq!(sub_task_result.status, vv_agent::ToolResultStatus::Success);
    let payload: serde_json::Value =
        serde_json::from_str(&sub_task_result.content).expect("sub-task payload");
    assert_eq!(payload["status"], "completed");
    assert_eq!(payload["agent_name"], "researcher");
    assert_eq!(payload["final_answer"], "child found vv-llm");
}

#[test]
fn runtime_forwards_stream_callback_to_runtime_backed_sub_agent() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let stream_callback: LlmStreamCallback = {
        let events = Arc::clone(&events);
        Arc::new(move |event| {
            events.lock().expect("events").push(event.clone());
        })
    };
    let log_events = Arc::new(Mutex::new(Vec::<(
        String,
        BTreeMap<String, serde_json::Value>,
    )>::new()));
    let log_sink = Arc::clone(&log_events);
    let mut runtime = AgentRuntime::new(StreamingSubAgentLlmClient::default());
    runtime.log_handler = Some(Arc::new(Mutex::new(Box::new(
        move |event: &str, payload: &BTreeMap<String, serde_json::Value>| {
            log_sink
                .lock()
                .expect("log events")
                .push((event.to_string(), payload.clone()));
        },
    ))));
    let mut task = AgentTask::new("parent_stream", "demo", "parent system", "delegate");
    task.sub_agents.insert(
        "researcher".to_string(),
        SubAgentConfig::new("demo", "research profile"),
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
        .expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert!(events.lock().expect("events").iter().any(|event| {
        event.get("event").and_then(serde_json::Value::as_str) == Some("assistant_delta")
            && event
                .get("content_delta")
                .and_then(serde_json::Value::as_str)
                == Some("checking")
            && event
                .get("sub_agent_name")
                .and_then(serde_json::Value::as_str)
                == Some("researcher")
    }));
    let log_events = log_events.lock().expect("log events");
    let log_event_names = log_events
        .iter()
        .map(|(event, _)| event.as_str())
        .collect::<Vec<_>>();
    assert!(log_event_names.contains(&"sub_agent_tool_call_started"));
    assert!(log_event_names.contains(&"sub_agent_tool_call_progress"));
    let sub_agent_delta = log_events
        .iter()
        .find(|(event, _)| event == "sub_agent_assistant_delta")
        .expect("sub-agent stream event in runtime logs");
    assert_eq!(sub_agent_delta.1["content_delta"], json!("checking"));
    assert_eq!(sub_agent_delta.1["sub_agent_name"], json!("researcher"));
    assert!(sub_agent_delta.1["task_id"].as_str().is_some());
    assert_eq!(
        sub_agent_delta.1["session_id"],
        sub_agent_delta.1["task_id"]
    );
    let sub_agent_progress = log_events
        .iter()
        .find(|(event, _)| event == "sub_agent_tool_call_progress")
        .expect("sub-agent tool progress event in runtime logs");
    assert_eq!(sub_agent_progress.1["tool_call_id"], json!("sub_tool_1"));
    assert_eq!(sub_agent_progress.1["function_name"], json!("bash"));
    assert_eq!(sub_agent_progress.1["arguments_chars"], json!(48));
    assert_eq!(sub_agent_progress.1["estimated_tokens"], json!(12));
    assert_eq!(sub_agent_progress.1["sub_agent_name"], json!("researcher"));
    assert!(sub_agent_progress.1["task_id"].as_str().is_some());
    assert_eq!(
        sub_agent_progress.1["session_id"],
        sub_agent_progress.1["task_id"]
    );
}

#[test]
fn runtime_rejects_sub_agent_model_mismatch_without_settings_file() {
    let mut sub_task_args = BTreeMap::new();
    sub_task_args.insert("agent_id".to_string(), json!("researcher"));
    sub_task_args.insert(
        "task_description".to_string(),
        json!("Use a different model"),
    );
    let mut parent_finish_args = BTreeMap::new();
    parent_finish_args.insert(
        "message".to_string(),
        json!("parent recorded child failure"),
    );

    let llm = ScriptedLlmClient::new(vec![
        LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::new(
                "parent_sub_call",
                "create_sub_task",
                sub_task_args,
            )],
        ),
        LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::new(
                "parent_finish",
                "task_finish",
                parent_finish_args,
            )],
        ),
    ]);
    let runtime = AgentRuntime::new(llm);
    let mut task = AgentTask::new(
        "parent_mismatch",
        "parent-model",
        "parent system",
        "delegate",
    );
    task.sub_agents.insert(
        "researcher".to_string(),
        SubAgentConfig::new("child-model", "research profile"),
    );

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    let sub_task_result = result
        .cycles
        .iter()
        .flat_map(|cycle| &cycle.tool_results)
        .find(|tool_result| tool_result.tool_call_id == "parent_sub_call")
        .expect("sub-task tool result");
    assert_eq!(sub_task_result.status, vv_agent::ToolResultStatus::Error);
    assert_eq!(
        sub_task_result.error_code.as_deref(),
        Some("sub_task_failed")
    );
    let payload: serde_json::Value =
        serde_json::from_str(&sub_task_result.content).expect("sub-task payload");
    assert_eq!(payload["status"], "failed");
    assert!(payload["error"]
        .as_str()
        .is_some_and(|error| error.contains("requires runtime settings_file")));
}

#[test]
fn runtime_adds_generated_prompt_sections_to_sub_agent_metadata() {
    let mut sub_task_args = BTreeMap::new();
    sub_task_args.insert("agent_id".to_string(), json!("researcher"));
    sub_task_args.insert(
        "task_description".to_string(),
        json!("Inspect generated prompt sections"),
    );
    let mut parent_finish_args = BTreeMap::new();
    parent_finish_args.insert("message".to_string(), json!("parent saw prompt metadata"));

    let llm = InspectingSubAgentPromptLlmClient::new(vec![
        LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::new(
                "parent_sub_call",
                "create_sub_task",
                sub_task_args,
            )],
        ),
        LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::new(
                "parent_finish",
                "task_finish",
                parent_finish_args,
            )],
        ),
    ]);
    let inspector = llm.clone();
    let runtime = AgentRuntime::new(llm);
    let mut task = AgentTask::new("parent_prompt", "demo", "parent system", "delegate");
    task.metadata.insert("language".to_string(), json!("zh-CN"));
    task.metadata.insert(
        "available_skills".to_string(),
        json!([{"name": "review-code", "description": "Review code"}]),
    );
    task.sub_agents.insert(
        "researcher".to_string(),
        SubAgentConfig::new("demo", "research profile"),
    );

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    let metadata = inspector
        .child_system_metadata()
        .expect("child system metadata");
    let sections = metadata["system_prompt_sections"]
        .as_array()
        .expect("system prompt sections");
    assert!(sections
        .iter()
        .any(|section| section["id"] == "agent_definition"));
    assert!(sections.iter().any(|section| section["id"] == "tools"));
}

#[test]
fn runtime_preserves_sub_agent_prompt_cache_metadata() {
    let mut sub_task_args = BTreeMap::new();
    sub_task_args.insert("agent_id".to_string(), json!("researcher"));
    sub_task_args.insert(
        "task_description".to_string(),
        json!("Inspect configured prompt sections"),
    );
    let mut parent_finish_args = BTreeMap::new();
    parent_finish_args.insert(
        "message".to_string(),
        json!("parent saw configured prompt metadata"),
    );

    let llm = InspectingSubAgentPromptLlmClient::new(vec![
        LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::new(
                "parent_sub_call",
                "create_sub_task",
                sub_task_args,
            )],
        ),
        LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::new(
                "parent_finish",
                "task_finish",
                parent_finish_args,
            )],
        ),
    ]);
    let inspector = llm.clone();
    let runtime = AgentRuntime::new(llm);
    let mut task = AgentTask::new(
        "parent_prompt_configured",
        "demo",
        "parent system",
        "delegate",
    );
    let mut sub_agent = SubAgentConfig::new("demo", "research profile");
    sub_agent
        .metadata
        .insert("anthropic_prompt_cache_enabled".to_string(), json!(true));
    sub_agent.metadata.insert(
        "system_prompt_sections".to_string(),
        json!([
            {"id": "core_identity", "text": "stable section", "stable": true}
        ]),
    );
    task.sub_agents.insert("researcher".to_string(), sub_agent);

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    let metadata = inspector
        .child_system_metadata()
        .expect("child system metadata");
    assert_eq!(metadata["anthropic_prompt_cache_enabled"], json!(true));
    let sections = metadata["system_prompt_sections"]
        .as_array()
        .expect("system prompt sections");
    assert_eq!(sections.len(), 1);
    assert_eq!(sections[0]["id"], json!("core_identity"));
}

#[test]
fn runtime_sub_agent_identity_metadata_cannot_be_overridden_by_request() {
    let mut sub_task_args = BTreeMap::new();
    sub_task_args.insert("agent_id".to_string(), json!("researcher"));
    sub_task_args.insert(
        "task_description".to_string(),
        json!("Inspect isolated metadata"),
    );
    let mut parent_finish_args = BTreeMap::new();
    parent_finish_args.insert("message".to_string(), json!("parent saw isolated metadata"));

    let llm = InspectingSubAgentPromptLlmClient::new(vec![
        LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::new(
                "parent_sub_call",
                "create_sub_task",
                sub_task_args,
            )],
        ),
        LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::new(
                "parent_finish",
                "task_finish",
                parent_finish_args,
            )],
        ),
    ]);
    let inspector = llm.clone();
    let runtime = AgentRuntime::new(llm);
    let mut task = AgentTask::new("parent_identity", "demo", "parent system", "delegate");
    let mut sub_agent = SubAgentConfig::new("demo", "research profile");
    sub_agent
        .metadata
        .insert("task_id".to_string(), json!("sub-agent-task-override"));
    sub_agent.metadata.insert(
        "session_id".to_string(),
        json!("sub-agent-session-override"),
    );
    sub_agent.metadata.insert(
        "browser_scope_key".to_string(),
        json!("sub-agent-browser-override"),
    );
    task.sub_agents.insert("researcher".to_string(), sub_agent);

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    let metadata = inspector
        .child_system_metadata()
        .expect("child system metadata");
    let task_id = metadata["task_id"].as_str().expect("task id");
    let session_id = metadata["session_id"].as_str().expect("session id");
    assert_ne!(task_id, "sub-agent-task-override");
    assert_ne!(session_id, "sub-agent-session-override");
    assert_eq!(session_id, task_id);
    assert_eq!(metadata["browser_scope_key"], metadata["session_id"]);
}

#[test]
fn runtime_seeds_skill_state_from_task_metadata() {
    let mut finish_args = BTreeMap::new();
    finish_args.insert("message".to_string(), json!("done"));
    let llm = ScriptedLlmClient::new(vec![LLMResponse::with_tool_calls(
        "finish",
        vec![ToolCall::new(
            "finish_skill_state",
            "task_finish",
            finish_args,
        )],
    )]);
    let runtime = AgentRuntime::new(llm);
    let mut task = AgentTask::new("skill_state", "demo", "system", "finish");
    task.metadata.insert(
        "available_skills".to_string(),
        json!([{"name": "demo", "description": "Demo skill"}]),
    );
    task.metadata
        .insert("active_skills".to_string(), json!(["already-active"]));

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(
        result.shared_state["available_skills"],
        json!([{"name": "demo", "description": "Demo skill"}])
    );
    assert_eq!(
        result.shared_state["active_skills"],
        json!(["already-active"])
    );
}

#[test]
fn runtime_keeps_initial_skill_state_over_task_metadata() {
    let mut finish_args = BTreeMap::new();
    finish_args.insert("message".to_string(), json!("done"));
    let llm = ScriptedLlmClient::new(vec![LLMResponse::with_tool_calls(
        "finish",
        vec![ToolCall::new(
            "finish_initial_skill_state",
            "task_finish",
            finish_args,
        )],
    )]);
    let runtime = AgentRuntime::new(llm);
    let mut task = AgentTask::new("initial_skill_state", "demo", "system", "finish");
    task.metadata.insert(
        "available_skills".to_string(),
        json!([{"name": "metadata-skill", "description": "Metadata skill"}]),
    );
    task.metadata
        .insert("active_skills".to_string(), json!(["metadata-active"]));
    task.initial_shared_state.insert(
        "available_skills".to_string(),
        json!([{"name": "state-skill", "description": "State skill"}]),
    );
    task.initial_shared_state
        .insert("active_skills".to_string(), json!(["state-active"]));

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(
        result.shared_state["available_skills"],
        json!([{"name": "state-skill", "description": "State skill"}])
    );
    assert_eq!(
        result.shared_state["active_skills"],
        json!(["state-active"])
    );
}

#[test]
fn runtime_can_poll_async_configured_sub_agent_status() {
    let mut sub_task_args = BTreeMap::new();
    sub_task_args.insert("agent_id".to_string(), json!("researcher"));
    sub_task_args.insert(
        "task_description".to_string(),
        json!("Collect async task facts"),
    );
    sub_task_args.insert("wait_for_completion".to_string(), json!(false));
    let llm = InspectingSubTaskStatusLlmClient::new(vec![LLMResponse::with_tool_calls(
        "",
        vec![ToolCall::new(
            "parent_async_sub_call",
            "create_sub_task",
            sub_task_args,
        )],
    )]);
    let inspector = llm.clone();
    let runtime = AgentRuntime::new(llm);
    let mut task = AgentTask::new("parent_async", "demo", "parent system", "delegate async");
    task.max_cycles = 50;
    task.sub_agents.insert(
        "researcher".to_string(),
        SubAgentConfig::new("demo", "research profile"),
    );

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(
        result.final_answer.as_deref(),
        Some("parent saw async child result")
    );
    assert!(inspector.status_payloads().iter().any(|payload| {
        payload["tasks"][0]["status"] == "completed"
            && payload["tasks"][0]["final_answer"] == "async child complete"
    }));
}

#[test]
fn runtime_can_continue_completed_async_sub_agent_session() {
    let mut sub_task_args = BTreeMap::new();
    sub_task_args.insert("agent_id".to_string(), json!("researcher"));
    sub_task_args.insert(
        "task_description".to_string(),
        json!("Collect async task facts"),
    );
    sub_task_args.insert("wait_for_completion".to_string(), json!(false));
    let llm = InspectingSubTaskContinuationLlmClient::new(vec![LLMResponse::with_tool_calls(
        "",
        vec![ToolCall::new(
            "parent_async_sub_call",
            "create_sub_task",
            sub_task_args,
        )],
    )]);
    let inspector = llm.clone();
    let runtime = AgentRuntime::new(llm);
    let mut task = AgentTask::new(
        "parent_async_continue",
        "demo",
        "parent system",
        "delegate async",
    );
    task.max_cycles = 50;
    task.sub_agents.insert(
        "researcher".to_string(),
        SubAgentConfig::new("demo", "research profile"),
    );

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(
        result.final_answer.as_deref(),
        Some("parent saw followed-up child result")
    );
    assert!(inspector.status_payloads().iter().any(|payload| {
        payload["interaction"]["action"] == "continued"
            && payload["tasks"][0]["final_answer"] == "follow-up child complete"
    }));
}

#[test]
fn runtime_hooks_can_patch_llm_request_and_tool_result_flow() {
    let hook = Arc::new(InspectingRuntimeHook::default());
    let llm = HookInspectingLlmClient::default();
    let inspector = llm.clone();
    let mut runtime = AgentRuntime::new(llm);
    runtime.hooks.push(hook.clone());

    let result = runtime
        .run(AgentTask::new("hook_task", "demo", "system", "original"))
        .expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(
        result.final_answer.as_deref(),
        Some("final answer patched by after_tool_call")
    );
    assert!(inspector.saw_hooked_message());
    assert_eq!(inspector.tool_schema_counts(), vec![0]);
    assert_eq!(
        hook.events(),
        vec![
            "before_llm",
            "after_llm",
            "before_tool_call",
            "after_tool_call"
        ]
    );
    assert_eq!(result.cycles[0].tool_results[0].tool_call_id, "hook_finish");
    assert!(result
        .messages
        .last()
        .expect("tool message")
        .content
        .contains("final answer patched by after_tool_call"));
}

#[test]
fn runtime_hooks_normalize_pending_tool_call_ids() {
    let hook = Arc::new(PendingToolCallIdHook);
    let llm = ScriptedLlmClient::new(vec![LLMResponse::with_tool_calls(
        "finish through pending hook",
        vec![ToolCall::new(
            "pending_hook_finish",
            "task_finish",
            BTreeMap::from([("message".to_string(), json!("original"))]),
        )],
    )]);
    let mut runtime = AgentRuntime::new(llm);
    runtime.hooks.push(hook);

    let result = runtime
        .run(AgentTask::new(
            "pending_hook_task",
            "demo",
            "system",
            "finish",
        ))
        .expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(
        result.final_answer.as_deref(),
        Some("finished by pending hook")
    );
    assert_eq!(
        result.cycles[0].tool_results[0].tool_call_id,
        "pending_hook_finish"
    );
    assert_eq!(
        result.messages.last().unwrap().tool_call_id.as_deref(),
        Some("pending_hook_finish")
    );
}

#[test]
fn before_tool_call_patch_accepts_direct_result_and_call_conversions() {
    let result_hook = Arc::new(DirectResultBeforeToolHook);
    let llm = ScriptedLlmClient::new(vec![LLMResponse::with_tool_calls(
        "finish through direct hook result",
        vec![ToolCall::new(
            "direct_result_finish",
            "task_finish",
            BTreeMap::from([("message".to_string(), json!("original"))]),
        )],
    )]);
    let mut runtime = AgentRuntime::new(llm);
    runtime.hooks.push(result_hook);

    let result = runtime
        .run(AgentTask::new(
            "direct_result_hook_task",
            "demo",
            "system",
            "go",
        ))
        .expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(
        result.final_answer.as_deref(),
        Some("finished by direct result hook")
    );

    let call_hook = Arc::new(PatchCallBeforeToolHook);
    let llm = ScriptedLlmClient::new(vec![LLMResponse::with_tool_calls(
        "finish through patched hook call",
        vec![ToolCall::new(
            "patch_call_finish",
            "task_finish",
            BTreeMap::from([("message".to_string(), json!("original"))]),
        )],
    )]);
    let mut runtime = AgentRuntime::new(llm);
    runtime.hooks.push(call_hook);

    let result = runtime
        .run(AgentTask::new(
            "patch_call_hook_task",
            "demo",
            "system",
            "go",
        ))
        .expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(
        result.final_answer.as_deref(),
        Some("finished by patched call hook")
    );
}

#[test]
fn runtime_short_circuit_tool_result_keeps_original_tool_call_id_after_call_patch() {
    let hook = Arc::new(PatchedCallAndBlankFinishHook);
    let llm = ScriptedLlmClient::new(vec![LLMResponse::with_tool_calls(
        "finish through patched short circuit",
        vec![ToolCall::new(
            "runtime_original_call",
            "task_finish",
            BTreeMap::from([("message".to_string(), json!("original"))]),
        )],
    )]);
    let mut runtime = AgentRuntime::new(llm);
    runtime.hooks.push(hook);

    let result = runtime
        .run(AgentTask::new(
            "patched_short_circuit_task",
            "demo",
            "system",
            "go",
        ))
        .expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(
        result.final_answer.as_deref(),
        Some("finished by patched short circuit")
    );
    assert_eq!(
        result.cycles[0].tool_results[0].tool_call_id,
        "runtime_original_call"
    );
    assert!(result.messages.iter().any(|message| {
        message.tool_call_id.as_deref() == Some("runtime_original_call")
            && message.content.contains("patched short circuit")
    }));
}

#[test]
fn runtime_emits_lifecycle_log_events() {
    let mut finish_args = BTreeMap::new();
    finish_args.insert("message".to_string(), json!("logged finish"));
    let llm = ScriptedLlmClient::new(vec![LLMResponse::with_tool_calls(
        "assistant log",
        vec![ToolCall::new("log_finish", "task_finish", finish_args)],
    )]);
    let events = Arc::new(Mutex::new(Vec::<(
        String,
        BTreeMap<String, serde_json::Value>,
    )>::new()));
    let sink = events.clone();
    let mut runtime = AgentRuntime::new(llm);
    runtime.log_handler = Some(Arc::new(Mutex::new(Box::new(
        move |event: &str, payload: &BTreeMap<String, serde_json::Value>| {
            sink.lock()
                .expect("events poisoned")
                .push((event.to_string(), payload.clone()));
        },
    ))));

    let result = runtime
        .run(AgentTask::new("log_task", "demo", "system", "finish"))
        .expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    let events = events.lock().expect("events poisoned").clone();
    let event_names = events
        .iter()
        .map(|(event, _)| event.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        event_names,
        vec![
            "run_started",
            "cycle_started",
            "cycle_llm_response",
            "tool_result",
            "run_completed"
        ]
    );
    assert_eq!(events[0].1["task_id"], "log_task");
    assert_eq!(events[0].1["model"], "demo");
    assert_eq!(events[2].1["assistant_message"], "assistant log");
    assert_eq!(events[2].1["tool_call_count"], 1);
    assert_eq!(events[3].1["tool_name"], "task_finish");
    assert_eq!(events[3].1["tool_call_id"], "log_finish");
    assert_eq!(events[3].1["directive"], "finish");
    assert_eq!(events[4].1["final_answer"], "logged finish");
}

#[test]
fn runtime_log_events_include_agent_previews() {
    let assistant_text = "assistant preview text ".repeat(4);
    let final_text = "final answer preview text ".repeat(4);
    let mut finish_args = BTreeMap::new();
    finish_args.insert("message".to_string(), json!(final_text.clone()));
    let llm = ScriptedLlmClient::new(vec![LLMResponse::with_tool_calls(
        assistant_text.clone(),
        vec![ToolCall::new("preview_finish", "task_finish", finish_args)],
    )]);
    let events = Arc::new(Mutex::new(Vec::<(
        String,
        BTreeMap<String, serde_json::Value>,
    )>::new()));
    let sink = events.clone();
    let mut runtime = AgentRuntime::new(llm);
    runtime.log_preview_chars = Some(10);
    runtime.log_handler = Some(Arc::new(Mutex::new(Box::new(
        move |event: &str, payload: &BTreeMap<String, serde_json::Value>| {
            sink.lock()
                .expect("events poisoned")
                .push((event.to_string(), payload.clone()));
        },
    ))));

    let result = runtime
        .run(AgentTask::new(
            "preview_task",
            "demo",
            "system",
            "finish with previews",
        ))
        .expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    let events = events.lock().expect("events poisoned").clone();
    let cycle_event = events
        .iter()
        .find(|(event, _)| event == "cycle_llm_response")
        .expect("cycle llm response");
    let tool_event = events
        .iter()
        .find(|(event, _)| event == "tool_result")
        .expect("tool result");
    let completed_event = events
        .iter()
        .find(|(event, _)| event == "run_completed")
        .expect("run completed");
    assert_eq!(cycle_event.1["assistant_message"], assistant_text);
    assert_eq!(
        cycle_event.1["assistant_preview"],
        preview_text_for_test(&assistant_text, Some(10))
    );
    assert_eq!(
        tool_event.1["content_preview"],
        preview_text_for_test(tool_event.1["content"].as_str().expect("content"), Some(10))
    );
    assert_eq!(
        completed_event.1["final_answer"],
        preview_text_for_test(&final_text, Some(10))
    );
}

#[test]
fn runtime_tool_result_event_keeps_full_content_by_default() {
    let long_title = "x".repeat(500);
    let todo_args = BTreeMap::from([(
        "todos".to_string(),
        json!([{"title": long_title, "status": "completed", "priority": "medium"}]),
    )]);
    let finish_args = BTreeMap::from([("message".to_string(), json!("ok"))]);
    let llm = ScriptedLlmClient::new(vec![
        LLMResponse::with_tool_calls(
            "write todo",
            vec![ToolCall::new("todo_long", "todo_write", todo_args)],
        ),
        LLMResponse::with_tool_calls(
            "done",
            vec![ToolCall::new("finish_long", "task_finish", finish_args)],
        ),
    ]);
    let events = Arc::new(Mutex::new(Vec::<(
        String,
        BTreeMap<String, serde_json::Value>,
    )>::new()));
    let sink = events.clone();
    let mut runtime = AgentRuntime::new(llm);
    runtime.log_handler = Some(Arc::new(Mutex::new(Box::new(
        move |event: &str, payload: &BTreeMap<String, serde_json::Value>| {
            sink.lock()
                .expect("events poisoned")
                .push((event.to_string(), payload.clone()));
        },
    ))));

    let mut task = AgentTask::new("task_long_tool_result", "demo", "system", "go");
    task.max_cycles = 4;
    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    let events = events.lock().expect("events poisoned").clone();
    let tool_event = events
        .iter()
        .find(|(event, _)| event == "tool_result")
        .expect("tool result");
    let full_content = tool_event.1["content"].as_str().expect("content");
    assert!(full_content.contains(&long_title));
    assert!(full_content.len() > 220);
    assert_eq!(
        tool_event.1["content_preview"].as_str().expect("preview"),
        full_content
    );
}

#[test]
fn runtime_emits_run_max_cycles_log_with_final_answer() {
    let llm = ScriptedLlmClient::new(vec![LLMResponse::new("step 1"), LLMResponse::new("step 2")]);
    let events = Arc::new(Mutex::new(Vec::<(
        String,
        BTreeMap<String, serde_json::Value>,
    )>::new()));
    let sink = events.clone();
    let mut runtime = AgentRuntime::new(llm);
    runtime.log_handler = Some(Arc::new(Mutex::new(Box::new(
        move |event: &str, payload: &BTreeMap<String, serde_json::Value>| {
            sink.lock()
                .expect("events poisoned")
                .push((event.to_string(), payload.clone()));
        },
    ))));
    let mut task = AgentTask::new("max_cycles_log", "demo", "system", "keep going");
    task.max_cycles = 2;

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::MaxCycles);
    let events = events.lock().expect("events poisoned").clone();
    let max_cycles = events
        .iter()
        .find(|(event, _)| event == "run_max_cycles")
        .expect("run max cycles event");
    assert_eq!(max_cycles.1["cycle"], json!(2));
    assert_eq!(
        max_cycles.1["final_answer"],
        json!("Reached max cycles without finish signal.")
    );
}

#[test]
fn runtime_controls_can_inject_messages_before_each_cycle() {
    let mut finish_args = BTreeMap::new();
    finish_args.insert("message".to_string(), json!("saw injected message"));
    let llm = ScriptedLlmClient::new(vec![LLMResponse::with_tool_calls(
        "finish",
        vec![ToolCall::new("finish_injected", "task_finish", finish_args)],
    )]);
    let runtime = AgentRuntime::new(llm);

    let result = runtime
        .run_with_controls(
            AgentTask::new("before_cycle_task", "demo", "system", "start"),
            RuntimeRunControls {
                before_cycle_messages: Some(Arc::new(|cycle_index, messages, shared_state| {
                    assert_eq!(cycle_index, 1);
                    assert_eq!(messages.len(), 2);
                    assert!(shared_state.contains_key("todo_list"));
                    vec![Message::user("injected before cycle")]
                })),
                ..RuntimeRunControls::default()
            },
        )
        .expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert!(result
        .messages
        .iter()
        .any(|message| message.content == "injected before cycle"));
}

#[test]
fn runtime_interruption_provider_skips_remaining_tools() {
    let mut registry = vv_agent::tools::build_default_registry();
    registry
        .register_tool(
            "_demo_noop",
            "noop",
            Arc::new(|_context, _arguments| ToolExecutionResult::success("", "{}")),
        )
        .expect("register noop");

    let mut finish_args = BTreeMap::new();
    finish_args.insert("message".to_string(), json!("done"));
    let llm = ScriptedLlmClient::new(vec![
        LLMResponse::with_tool_calls(
            "two tools",
            vec![
                ToolCall::new("t1", "_demo_noop", BTreeMap::new()),
                ToolCall::new("t2", "_demo_noop", BTreeMap::new()),
            ],
        ),
        LLMResponse::with_tool_calls(
            "finish",
            vec![ToolCall::new(
                "finish_after_steer",
                "task_finish",
                finish_args,
            )],
        ),
    ]);
    let runtime = AgentRuntime::new(llm).with_tool_registry(registry);
    let used = Arc::new(Mutex::new(false));
    let provider_used = used.clone();
    let events = Arc::new(Mutex::new(Vec::<(
        String,
        BTreeMap<String, serde_json::Value>,
    )>::new()));
    let sink = events.clone();

    let mut task = AgentTask::new("steer_skip", "demo", "system", "go");
    task.max_cycles = 4;
    task.extra_tool_names = vec!["_demo_noop".to_string()];

    let result = runtime
        .run_with_controls(
            task,
            RuntimeRunControls {
                interruption_messages: Some(Arc::new(move || {
                    let mut used = provider_used.lock().expect("provider flag");
                    if *used {
                        Vec::new()
                    } else {
                        *used = true;
                        vec![Message::user("STEER_NOW")]
                    }
                })),
                log_handler: Some(Arc::new(move |event, payload| {
                    sink.lock()
                        .expect("events")
                        .push((event.to_string(), payload.clone()));
                })),
                ..RuntimeRunControls::default()
            },
        )
        .expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(
        result.cycles[0].tool_results[1].error_code.as_deref(),
        Some("skipped_due_to_steering")
    );
    assert!(result
        .messages
        .iter()
        .any(|message| message.content == "STEER_NOW"));
    let events = events.lock().expect("events").clone();
    assert!(events.iter().any(|(event, _)| event == "run_steered"));
}

#[test]
fn cancellation_token_propagates_to_children_and_runtime() {
    let parent = CancellationToken::default();
    let child = parent.child();
    assert!(!parent.is_cancelled());
    assert!(!child.is_cancelled());

    parent.cancel();

    assert!(parent.is_cancelled());
    assert!(child.is_cancelled());

    let runtime = AgentRuntime::new(ScriptedLlmClient::new(vec![LLMResponse::new(
        "should not be used",
    )]));
    let result = runtime
        .run_with_controls(
            AgentTask::new("cancel_task", "demo", "system", "start"),
            RuntimeRunControls {
                cancellation_token: Some(parent),
                ..RuntimeRunControls::default()
            },
        )
        .expect("cancelled result");

    assert_eq!(result.status, AgentStatus::Failed);
    assert!(result
        .error
        .as_deref()
        .unwrap_or_default()
        .contains("cancelled"));
    assert!(result.cycles.is_empty());
}

#[test]
fn cancellation_token_callbacks_match_agent_semantics() {
    let token = CancellationToken::default();
    assert!(!token.cancelled());
    assert!(token.check().is_ok());

    let calls = Arc::new(Mutex::new(Vec::new()));
    let callback_calls = Arc::clone(&calls);
    token.on_cancel(move || {
        callback_calls
            .lock()
            .expect("callback calls lock")
            .push("first");
    });
    assert!(calls.lock().expect("callback calls lock").is_empty());

    token.cancel();
    token.cancel();

    assert!(token.cancelled());
    assert_eq!(*calls.lock().expect("callback calls lock"), vec!["first"]);
    assert!(token.check().is_err());

    let immediate_calls = Arc::new(Mutex::new(Vec::new()));
    let callback_calls = Arc::clone(&immediate_calls);
    token.on_cancel(move || {
        callback_calls
            .lock()
            .expect("immediate callback calls lock")
            .push("immediate");
    });
    assert_eq!(
        *immediate_calls
            .lock()
            .expect("immediate callback calls lock"),
        vec!["immediate"]
    );

    let parent = CancellationToken::default();
    let child = parent.child();
    let grandchild = child.child();
    child.cancel();
    assert!(child.cancelled());
    assert!(child.is_cancelled());
    assert!(grandchild.is_cancelled());
    assert!(!parent.cancelled());
    assert!(!parent.is_cancelled());
}

#[test]
fn execution_context_cancellation_token_is_honored_by_runtime() {
    let token = CancellationToken::default();
    token.cancel();
    let context = vv_agent::ExecutionContext::default().with_cancellation_token(token);
    let runtime = AgentRuntime::new(ScriptedLlmClient::new(vec![LLMResponse::new(
        "should not be used",
    )]));

    let result = runtime
        .run_with_controls(
            AgentTask::new("ctx_cancel_task", "demo", "system", "start"),
            RuntimeRunControls {
                execution_context: Some(context),
                ..RuntimeRunControls::default()
            },
        )
        .expect("cancelled result");

    assert_eq!(result.status, AgentStatus::Failed);
    assert!(result
        .error
        .as_deref()
        .unwrap_or_default()
        .contains("cancelled"));
    assert!(result.cycles.is_empty());
}

#[test]
fn runtime_compacts_memory_before_large_follow_up_cycle() {
    let workspace = tempfile::tempdir().expect("workspace");
    let large_tool_payload = "tool output ".repeat(300);
    let llm = MemoryCompactionInspectingLlmClient::new(large_tool_payload);
    let inspector = llm.clone();
    let mut runtime = AgentRuntime::new(llm);
    runtime.default_workspace = Some(workspace.path().to_path_buf());
    runtime.workspace_backend = Arc::new(vv_agent::workspace::LocalWorkspaceBackend::new(
        workspace.path(),
    ));
    let mut task = AgentTask::new("memory_task", "demo", "system", "inspect memory");
    task.memory_compact_threshold = 20;
    task.metadata
        .insert("model_context_window".to_string(), json!(120));
    task.metadata
        .insert("reserved_output_tokens".to_string(), json!(10));
    task.metadata
        .insert("autocompact_buffer_tokens".to_string(), json!(0));

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert!(result.cycles.iter().any(|cycle| cycle.memory_compacted));
    let second_request = inspector.second_request_messages();
    assert!(
        second_request
            .iter()
            .any(|message| message.content.contains("<Compressed Agent Memory>")),
        "second request did not contain compressed memory: {second_request:#?}"
    );
    assert!(
        second_request.len() <= 3,
        "compacted request should keep system, summary, and latest continuation at most"
    );
}

#[test]
fn runtime_uses_previous_prompt_tokens_for_memory_compaction() {
    let llm = PromptTokenCompactionInspectingLlmClient::default();
    let inspector = llm.clone();
    let runtime = AgentRuntime::new(llm);
    let mut task = AgentTask::new("usage_memory_task", "demo", "system", "short request");
    task.max_cycles = 2;
    task.metadata
        .insert("model_context_window".to_string(), json!(120));
    task.metadata
        .insert("reserved_output_tokens".to_string(), json!(10));
    task.metadata
        .insert("autocompact_buffer_tokens".to_string(), json!(10));

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    let second_request = inspector.second_request_messages();
    assert!(
        second_request
            .iter()
            .any(|message| message.content.contains("<Compressed Agent Memory>")),
        "runtime ignored previous provider prompt_tokens when preparing the next cycle: {second_request:#?}"
    );
}

#[test]
fn runtime_hooks_can_patch_messages_before_memory_compaction() {
    let workspace = tempfile::tempdir().expect("workspace");
    let large_tool_payload = "tool output ".repeat(300);
    let llm = MemoryCompactionInspectingLlmClient::new(large_tool_payload);
    let inspector = llm.clone();
    let hook = Arc::new(BeforeMemoryCompactHook::default());
    let mut runtime = AgentRuntime::new(llm);
    runtime.default_workspace = Some(workspace.path().to_path_buf());
    runtime.workspace_backend = Arc::new(vv_agent::workspace::LocalWorkspaceBackend::new(
        workspace.path(),
    ));
    runtime.hooks.push(hook.clone());
    let mut task = AgentTask::new("pre_compact_hook_task", "demo", "system", "inspect memory");
    task.memory_compact_threshold = 20;
    task.metadata
        .insert("model_context_window".to_string(), json!(120));
    task.metadata
        .insert("reserved_output_tokens".to_string(), json!(10));
    task.metadata
        .insert("autocompact_buffer_tokens".to_string(), json!(0));

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(hook.cycle_indexes(), vec![1, 2]);
    let second_request = inspector.second_request_messages();
    assert!(
        second_request
            .iter()
            .any(|message| message.content.contains("hook-added-before-memory-compact")),
        "second request did not include before-memory-compaction hook marker: {second_request:#?}"
    );
}

#[test]
fn runtime_injects_session_memory_context_after_compaction() {
    let workspace = tempfile::tempdir().expect("workspace");
    let large_tool_payload = "tool output ".repeat(300);
    let llm = MemoryCompactionInspectingLlmClient::new(large_tool_payload);
    let inspector = llm.clone();
    let mut runtime = AgentRuntime::new(llm);
    runtime.default_workspace = Some(workspace.path().to_path_buf());
    runtime.workspace_backend = Arc::new(vv_agent::workspace::LocalWorkspaceBackend::new(
        workspace.path(),
    ));
    let mut task = AgentTask::new("session_memory_task", "demo", "system", "inspect memory");
    task.memory_compact_threshold = 20;
    task.metadata
        .insert("model_context_window".to_string(), json!(120));
    task.metadata
        .insert("reserved_output_tokens".to_string(), json!(10));
    task.metadata
        .insert("autocompact_buffer_tokens".to_string(), json!(0));
    task.metadata
        .insert("session_memory_enabled".to_string(), json!(true));
    task.metadata
        .insert("session_memory_min_tokens".to_string(), json!(1));
    task.metadata
        .insert("session_memory_min_text_messages".to_string(), json!(1));
    task.metadata.insert(
        "session_memory_seed".to_string(),
        json!([
            {"category":"key_fact","content":"sub-agent findings survive compaction","importance":9,"source_cycle":0}
        ]),
    );

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    let second_request = inspector.second_request_messages();
    assert!(
        second_request
            .first()
            .is_some_and(|message| message.content.contains("<Session Memory>")
                && message
                    .content
                    .contains("sub-agent findings survive compaction")),
        "second request did not include session memory context: {second_request:#?}"
    );
}

#[test]
fn runtime_loads_session_memory_by_default() {
    let workspace = tempfile::tempdir().expect("workspace");
    let storage_dir = workspace
        .path()
        .join(".memory/session/session_memory_default_task");
    fs::create_dir_all(&storage_dir).expect("session memory dir");
    fs::write(
        storage_dir.join("session_memory.json"),
        json!({
            "entries": [{
                "category": "key_fact",
                "content": "default session memory is loaded",
                "source_cycle": 3,
                "importance": 9
            }],
            "last_extracted_message_index": -1,
            "tokens_at_last_extraction": 0,
            "initialized": true
        })
        .to_string(),
    )
    .expect("session memory file");
    let llm = DefaultSessionMemoryInspectingLlmClient::default();
    let inspector = llm.clone();
    let mut runtime = AgentRuntime::new(llm);
    runtime.default_workspace = Some(workspace.path().to_path_buf());
    runtime.workspace_backend = Arc::new(vv_agent::workspace::LocalWorkspaceBackend::new(
        workspace.path(),
    ));
    let mut task = AgentTask::new(
        "session_memory_default_task",
        "demo",
        "system",
        "inspect memory",
    );
    task.max_cycles = 1;
    task.no_tool_policy = vv_agent::NoToolPolicy::Finish;

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    let first_request = inspector.first_request_messages();
    assert!(
        first_request
            .first()
            .is_some_and(|message| message.content.contains("<Session Memory>")
                && message.content.contains("default session memory is loaded")),
        "runtime did not load session memory by default: {first_request:#?}"
    );
}

#[test]
fn runtime_disables_session_memory_with_integer_zero() {
    let workspace = tempfile::tempdir().expect("workspace");
    let storage_dir = workspace.path().join(".memory/session/int_disabled_task");
    fs::create_dir_all(&storage_dir).expect("session memory dir");
    fs::write(
        storage_dir.join("session_memory.json"),
        json!({
            "entries": [{
                "category": "key_fact",
                "content": "integer zero should disable session memory",
                "source_cycle": 1,
                "importance": 9
            }],
            "last_extracted_message_index": -1,
            "tokens_at_last_extraction": 0,
            "initialized": true
        })
        .to_string(),
    )
    .expect("session memory file");
    let llm = DefaultSessionMemoryInspectingLlmClient::default();
    let inspector = llm.clone();
    let mut runtime = AgentRuntime::new(llm);
    runtime.default_workspace = Some(workspace.path().to_path_buf());
    runtime.workspace_backend = Arc::new(vv_agent::workspace::LocalWorkspaceBackend::new(
        workspace.path(),
    ));
    let mut task = AgentTask::new("int_disabled_task", "demo", "system", "inspect memory");
    task.max_cycles = 1;
    task.no_tool_policy = vv_agent::NoToolPolicy::Finish;
    task.metadata
        .insert("session_memory_enabled".to_string(), json!(0));

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    let first_request = inspector.first_request_messages();
    assert!(
        first_request
            .iter()
            .all(|message| !message.content.contains("<Session Memory>")
                && !message
                    .content
                    .contains("integer zero should disable session memory")),
        "integer 0 should disable session memory: {first_request:#?}"
    );
}

#[test]
fn runtime_scopes_session_memory_by_session_id_metadata() {
    let workspace = tempfile::tempdir().expect("workspace");
    let storage_dir = workspace.path().join(".memory/session/session-scope");
    fs::create_dir_all(&storage_dir).expect("session memory dir");
    fs::write(
        storage_dir.join("session_memory.json"),
        json!({
            "entries": [{
                "category": "key_fact",
                "content": "session scoped memory is loaded",
                "source_cycle": 2,
                "importance": 8
            }],
            "last_extracted_message_index": -1,
            "tokens_at_last_extraction": 0,
            "initialized": true
        })
        .to_string(),
    )
    .expect("session memory file");

    let llm = DefaultSessionMemoryInspectingLlmClient::default();
    let inspector = llm.clone();
    let mut runtime = AgentRuntime::new(llm);
    runtime.default_workspace = Some(workspace.path().to_path_buf());
    runtime.workspace_backend = Arc::new(vv_agent::workspace::LocalWorkspaceBackend::new(
        workspace.path(),
    ));
    let mut task = AgentTask::new(
        "fresh_task_id_for_same_session",
        "demo",
        "system",
        "inspect scoped memory",
    );
    task.max_cycles = 1;
    task.no_tool_policy = vv_agent::NoToolPolicy::Finish;
    task.metadata
        .insert("session_id".to_string(), json!("session-scope"));

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    let first_request = inspector.first_request_messages();
    assert!(
        first_request
            .first()
            .is_some_and(|message| message.content.contains("<Session Memory>")
                && message.content.contains("session scoped memory is loaded")),
        "runtime did not use session_id as session-memory storage scope: {first_request:#?}"
    );
}

#[test]
fn runtime_uses_memory_summary_metadata_model_for_session_extraction() {
    let workspace = tempfile::tempdir().expect("workspace");
    let settings_file = workspace.path().join("local_settings.py");
    fs::write(
        &settings_file,
        r#"
DEFAULT_USER_MEMORY_SUMMARIZE_BACKEND = "settings-backend"
DEFAULT_USER_MEMORY_SUMMARIZE_MODEL = "settings-model"
"#,
    )
    .expect("settings file");
    let llm = SummaryModelInspectingLlmClient::default();
    let inspector = llm.clone();
    let mut runtime = AgentRuntime::new(llm)
        .with_settings_file(settings_file)
        .with_default_backend("fallback-backend");
    runtime.default_workspace = Some(workspace.path().to_path_buf());
    runtime.workspace_backend = Arc::new(vv_agent::workspace::LocalWorkspaceBackend::new(
        workspace.path(),
    ));
    let mut task = AgentTask::new(
        "summary_model_priority_task",
        "task-model",
        "system",
        "inspect memory",
    );
    task.memory_compact_threshold = 1;
    task.no_tool_policy = vv_agent::NoToolPolicy::Finish;
    task.metadata
        .insert("memory_summary_model".to_string(), json!("metadata-model"));
    task.metadata.insert(
        "memory_summary_backend".to_string(),
        json!("metadata-backend"),
    );
    task.metadata
        .insert("session_memory_min_tokens".to_string(), json!(1));
    task.metadata
        .insert("session_memory_min_text_messages".to_string(), json!(1));

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(
        inspector.extraction_model(),
        Some("metadata-model".to_string())
    );
}

#[test]
fn runtime_uses_local_memory_summary_model_defaults() {
    let workspace = tempfile::tempdir().expect("workspace");
    let settings_file = workspace.path().join("local_settings.py");
    fs::write(
        &settings_file,
        r#"
DEFAULT_USER_MEMORY_SUMMARIZE_BACKEND = "settings-backend"
DEFAULT_USER_MEMORY_SUMMARIZE_MODEL = "settings-model"
"#,
    )
    .expect("settings file");
    let llm = SummaryModelInspectingLlmClient::default();
    let inspector = llm.clone();
    let mut runtime = AgentRuntime::new(llm)
        .with_settings_file(settings_file)
        .with_default_backend("fallback-backend");
    runtime.default_workspace = Some(workspace.path().to_path_buf());
    runtime.workspace_backend = Arc::new(vv_agent::workspace::LocalWorkspaceBackend::new(
        workspace.path(),
    ));
    let mut task = AgentTask::new(
        "summary_model_settings_task",
        "task-model",
        "system",
        "inspect memory",
    );
    task.memory_compact_threshold = 1;
    task.no_tool_policy = vv_agent::NoToolPolicy::Finish;
    task.metadata
        .insert("session_memory_min_tokens".to_string(), json!(1));
    task.metadata
        .insert("session_memory_min_text_messages".to_string(), json!(1));

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(
        inspector.extraction_model(),
        Some("settings-model".to_string())
    );
}

#[test]
fn runtime_uses_settings_model_token_limits_for_direct_runtime_memory() {
    let workspace = tempfile::tempdir().expect("workspace");
    let settings_file = workspace.path().join("llm_settings.json");
    fs::write(
        &settings_file,
        json!({
            "VERSION": "2",
            "endpoints": [{
                "id": "deepseek-primary",
                "api_key": "sk-test",
                "api_base": "https://api.deepseek.test"
            }],
            "backends": {
                "deepseek": {
                    "models": {
                        "deepseek-v4-pro": {
                            "id": "deepseek-v4-pro",
                            "endpoints": ["deepseek-primary"],
                            "context_length": 1000,
                            "max_output_tokens": 100
                        }
                    }
                }
            }
        })
        .to_string(),
    )
    .expect("settings");

    let llm = DefaultSessionMemoryInspectingLlmClient::default();
    let inspector = llm.clone();
    let runtime = AgentRuntime::new(llm)
        .with_settings_file(settings_file)
        .with_default_backend("deepseek");
    let mut task = AgentTask::new(
        "direct_runtime_limits",
        "deepseek-v4-pro",
        "system",
        "Please remember this direct runtime token-limit check.",
    );
    task.max_cycles = 1;
    task.no_tool_policy = vv_agent::NoToolPolicy::Finish;
    task.memory_compact_threshold = 0;
    task.memory_threshold_percentage = 1;
    task.metadata
        .insert("include_memory_warning".to_string(), json!(true));
    task.metadata
        .insert("autocompact_buffer_tokens".to_string(), json!(0));
    task.metadata
        .insert("session_memory_enabled".to_string(), json!(false));

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    let first_request = inspector.first_request_messages();
    assert!(
        first_request
            .iter()
            .any(|message| message.content.contains("当前记忆已使用容量超过 1%")),
        "direct runtime did not use settings-derived token limits for memory warning: {first_request:#?}"
    );
}

#[test]
fn runtime_microcompacts_before_full_memory_compaction() {
    let workspace = tempfile::tempdir().expect("workspace");
    let large_tool_payload = "tool output ".repeat(300);
    let llm = MicrocompactInspectingLlmClient::new(large_tool_payload);
    let inspector = llm.clone();
    let mut runtime = AgentRuntime::new(llm);
    runtime.default_workspace = Some(workspace.path().to_path_buf());
    runtime.workspace_backend = Arc::new(vv_agent::workspace::LocalWorkspaceBackend::new(
        workspace.path(),
    ));
    let mut task = AgentTask::new("microcompact_task", "demo", "system", "inspect memory");
    task.memory_compact_threshold = 10_000;
    task.metadata
        .insert("model_context_window".to_string(), json!(20_000));
    task.metadata
        .insert("reserved_output_tokens".to_string(), json!(0));
    task.metadata
        .insert("autocompact_buffer_tokens".to_string(), json!(0));
    task.metadata
        .insert("microcompact_trigger_ratio".to_string(), json!(0.01));
    task.metadata
        .insert("microcompact_keep_recent_cycles".to_string(), json!(0));
    task.metadata
        .insert("microcompact_min_result_length".to_string(), json!(200));

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    let second_request = inspector.third_request_messages();
    assert!(
        second_request
            .iter()
            .any(|message| message.content == CLEARED_MARKER),
        "second request did not include microcompacted tool content: {second_request:#?}"
    );
    assert!(
        second_request
            .iter()
            .all(|message| !message.content.contains("<Compressed Agent Memory>")),
        "microcompact should avoid full summary before threshold: {second_request:#?}"
    );
}

#[test]
fn runtime_respects_configured_microcompact_tool_allowlist() {
    let workspace = tempfile::tempdir().expect("workspace");
    let large_tool_payload = "tool output ".repeat(300);
    let llm = MicrocompactInspectingLlmClient::new(large_tool_payload);
    let inspector = llm.clone();
    let mut runtime = AgentRuntime::new(llm);
    runtime.default_workspace = Some(workspace.path().to_path_buf());
    runtime.workspace_backend = Arc::new(vv_agent::workspace::LocalWorkspaceBackend::new(
        workspace.path(),
    ));
    let mut task = AgentTask::new(
        "microcompact_allowlist_task",
        "demo",
        "system",
        "inspect memory",
    );
    task.memory_compact_threshold = 10_000;
    task.metadata
        .insert("model_context_window".to_string(), json!(20_000));
    task.metadata
        .insert("reserved_output_tokens".to_string(), json!(0));
    task.metadata
        .insert("autocompact_buffer_tokens".to_string(), json!(0));
    task.metadata
        .insert("microcompact_trigger_ratio".to_string(), json!(0.01));
    task.metadata
        .insert("microcompact_keep_recent_cycles".to_string(), json!(0));
    task.metadata
        .insert("microcompact_min_result_length".to_string(), json!(200));
    task.metadata.insert(
        "microcompact_compactable_tools".to_string(),
        json!(["read_file"]),
    );

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    let third_request = inspector.third_request_messages();
    assert!(
        third_request
            .iter()
            .all(|message| message.content != CLEARED_MARKER),
        "bash output should not be microcompacted when only read_file is allowlisted: {third_request:#?}"
    );
    assert!(
        third_request
            .iter()
            .any(|message| message.content.contains("tool output")),
        "original bash output should remain available: {third_request:#?}"
    );
}

#[test]
fn runtime_parses_string_float_microcompact_ratio() {
    let workspace = tempfile::tempdir().expect("workspace");
    let large_tool_payload = "tool output ".repeat(300);
    let llm = MicrocompactInspectingLlmClient::new(large_tool_payload);
    let inspector = llm.clone();
    let mut runtime = AgentRuntime::new(llm);
    runtime.default_workspace = Some(workspace.path().to_path_buf());
    runtime.workspace_backend = Arc::new(vv_agent::workspace::LocalWorkspaceBackend::new(
        workspace.path(),
    ));
    let mut task = AgentTask::new(
        "microcompact_string_ratio_task",
        "demo",
        "system",
        "inspect memory",
    );
    task.memory_compact_threshold = 10_000;
    task.metadata
        .insert("model_context_window".to_string(), json!(20_000));
    task.metadata
        .insert("reserved_output_tokens".to_string(), json!(0));
    task.metadata
        .insert("autocompact_buffer_tokens".to_string(), json!(0));
    task.metadata
        .insert("microcompact_trigger_ratio".to_string(), json!("0.01"));
    task.metadata
        .insert("microcompact_keep_recent_cycles".to_string(), json!(0));
    task.metadata
        .insert("microcompact_min_result_length".to_string(), json!(200));

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    let third_request = inspector.third_request_messages();
    assert!(
        third_request
            .iter()
            .any(|message| message.content == CLEARED_MARKER),
        "string float microcompact ratio should trigger the same compaction: {third_request:#?}"
    );
}

#[test]
fn runtime_retries_prompt_too_long_with_emergency_compaction() {
    let llm = PromptTooLongRetryLlmClient::default();
    let inspector = llm.clone();
    let runtime = AgentRuntime::new(llm);
    let mut task = AgentTask::new("ptl_task", "demo", "system", "finish after retry");
    task.no_tool_policy = vv_agent::NoToolPolicy::Finish;
    task.memory_compact_threshold = 10_000;
    task.metadata
        .insert("model_context_window".to_string(), json!(20_000));
    task.metadata
        .insert("reserved_output_tokens".to_string(), json!(0));
    task.metadata
        .insert("autocompact_buffer_tokens".to_string(), json!(0));
    task.metadata
        .insert("memory_keep_recent_messages".to_string(), json!(2));

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(result.final_answer.as_deref(), Some("done after retry"));
    assert!(result.cycles.iter().any(|cycle| cycle.memory_compacted));
    assert_eq!(inspector.request_count(), 3);
    let final_request = inspector.final_request_messages();
    assert!(
        final_request.len() < 4,
        "final retry should use emergency-compacted messages: {final_request:#?}"
    );
}

#[test]
fn runtime_returns_compaction_exhausted_after_prompt_too_long_retries() {
    let llm = AlwaysPromptTooLongLlmClient::default();
    let inspector = llm.clone();
    let runtime = AgentRuntime::new(llm);
    let mut task = AgentTask::new("ptl_exhausted", "demo", "system", "never fits");
    task.memory_compact_threshold = 10_000;
    task.metadata
        .insert("model_context_window".to_string(), json!(20_000));
    task.metadata
        .insert("reserved_output_tokens".to_string(), json!(0));
    task.metadata
        .insert("autocompact_buffer_tokens".to_string(), json!(0));

    let error = runtime.run(task).expect_err("compaction exhausted error");

    match error {
        LlmError::CompactionExhausted(error) => {
            assert_eq!(error.attempts, 4);
            assert!(error
                .last_error
                .as_deref()
                .is_some_and(|message| message.contains("Prompt is too long")));
        }
        other => panic!("expected CompactionExhausted, got {other:?}"),
    }
    assert_eq!(inspector.request_count(), 4);
}

#[test]
fn runtime_extracts_session_memory_with_default_llm_callback() {
    let workspace = tempfile::tempdir().expect("workspace");
    let large_tool_payload = "tool output ".repeat(300);
    let llm = SessionMemoryExtractingLlmClient::new(large_tool_payload);
    let inspector = llm.clone();
    let mut runtime = AgentRuntime::new(llm);
    runtime.default_workspace = Some(workspace.path().to_path_buf());
    runtime.workspace_backend = Arc::new(vv_agent::workspace::LocalWorkspaceBackend::new(
        workspace.path(),
    ));
    let mut task = AgentTask::new(
        "session_memory_extract_task",
        "demo",
        "system",
        "inspect memory",
    );
    task.memory_compact_threshold = 20;
    task.metadata
        .insert("model_context_window".to_string(), json!(120));
    task.metadata
        .insert("reserved_output_tokens".to_string(), json!(10));
    task.metadata
        .insert("autocompact_buffer_tokens".to_string(), json!(0));
    task.metadata
        .insert("session_memory_enabled".to_string(), json!(true));
    task.metadata
        .insert("session_memory_min_tokens".to_string(), json!(1));
    task.metadata
        .insert("session_memory_min_text_messages".to_string(), json!(1));

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(inspector.extraction_prompt_count(), 2);
    let second_request = inspector.second_request_messages();
    assert!(
        second_request
            .first()
            .is_some_and(|message| message.content.contains("<Session Memory>")
                && message
                    .content
                    .contains("default callback preserved this fact")),
        "second request did not include extracted session memory: {second_request:#?}"
    );
}

#[derive(Clone)]
struct InspectingImageLlmClient {
    responses: Arc<Mutex<VecDeque<LLMResponse>>>,
    saw_image_message: Arc<Mutex<bool>>,
}

impl InspectingImageLlmClient {
    fn new(first: LLMResponse, second: LLMResponse) -> Self {
        Self {
            responses: Arc::new(Mutex::new(VecDeque::from([first, second]))),
            saw_image_message: Arc::new(Mutex::new(false)),
        }
    }

    fn saw_image_message(&self) -> bool {
        *self.saw_image_message.lock().expect("inspector poisoned")
    }
}

impl LlmClient for InspectingImageLlmClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        if request.messages.iter().any(|message| {
            message.image_url.as_deref().is_some_and(|image_url| {
                message.role == vv_agent::MessageRole::User
                    && message.content.starts_with("[Image loaded]")
                    && image_url.starts_with("data:image/png;base64,")
            })
        }) {
            *self.saw_image_message.lock().expect("inspector poisoned") = true;
        }
        self.responses
            .lock()
            .map_err(|_| LlmError::Request("inspector poisoned".to_string()))?
            .pop_front()
            .ok_or(LlmError::ScriptExhausted)
    }
}

#[derive(Clone)]
struct MessageOrderInspectingLlmClient {
    responses: Arc<Mutex<VecDeque<LLMResponse>>>,
    requests: Arc<Mutex<Vec<Vec<Message>>>>,
}

impl MessageOrderInspectingLlmClient {
    fn new(first: LLMResponse, second: LLMResponse) -> Self {
        Self {
            responses: Arc::new(Mutex::new(VecDeque::from([first, second]))),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn second_request_messages(&self) -> Vec<Message> {
        self.requests
            .lock()
            .expect("requests poisoned")
            .get(1)
            .cloned()
            .expect("second request")
    }
}

impl LlmClient for MessageOrderInspectingLlmClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        self.requests
            .lock()
            .map_err(|_| LlmError::Request("requests poisoned".to_string()))?
            .push(request.messages);
        self.responses
            .lock()
            .map_err(|_| LlmError::Request("responses poisoned".to_string()))?
            .pop_front()
            .ok_or(LlmError::ScriptExhausted)
    }
}

#[derive(Clone)]
struct InspectingSubAgentPromptLlmClient {
    responses: Arc<Mutex<VecDeque<LLMResponse>>>,
    child_system_metadata: Arc<Mutex<Option<BTreeMap<String, serde_json::Value>>>>,
}

impl InspectingSubAgentPromptLlmClient {
    fn new(responses: Vec<LLMResponse>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(VecDeque::from(responses))),
            child_system_metadata: Arc::new(Mutex::new(None)),
        }
    }

    fn child_system_metadata(&self) -> Option<BTreeMap<String, serde_json::Value>> {
        self.child_system_metadata
            .lock()
            .expect("child metadata poisoned")
            .clone()
    }
}

impl LlmClient for InspectingSubAgentPromptLlmClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        let is_child_request = request
            .messages
            .first()
            .is_some_and(|message| message.content.contains("research profile"));
        if is_child_request {
            let metadata = request
                .messages
                .first()
                .map(|message| message.metadata.clone())
                .unwrap_or_default();
            *self
                .child_system_metadata
                .lock()
                .expect("child metadata poisoned") = Some(metadata);
            return Ok(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::new(
                    "child_prompt_finish",
                    "task_finish",
                    BTreeMap::from([("message".to_string(), json!("child saw prompt"))]),
                )],
            ));
        }

        self.responses
            .lock()
            .map_err(|_| LlmError::Request("inspector poisoned".to_string()))?
            .pop_front()
            .ok_or(LlmError::ScriptExhausted)
    }
}

#[derive(Clone)]
struct InspectingSubTaskStatusLlmClient {
    responses: Arc<Mutex<VecDeque<LLMResponse>>>,
    status_payloads: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl InspectingSubTaskStatusLlmClient {
    fn new(responses: Vec<LLMResponse>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(VecDeque::from(responses))),
            status_payloads: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn status_payloads(&self) -> Vec<serde_json::Value> {
        self.status_payloads
            .lock()
            .expect("status payloads poisoned")
            .clone()
    }
}

impl LlmClient for InspectingSubTaskStatusLlmClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        let is_child_request = request
            .messages
            .first()
            .is_some_and(|message| message.content.contains("research profile"));
        if is_child_request {
            return Ok(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::new(
                    "child_async_finish",
                    "task_finish",
                    BTreeMap::from([("message".to_string(), json!("async child complete"))]),
                )],
            ));
        }
        if !is_child_request {
            let latest_async_task_id = request
                .messages
                .iter()
                .rev()
                .filter_map(|message| {
                    if message.role != vv_agent::MessageRole::Tool
                        || message.tool_call_id.as_deref() != Some("parent_async_sub_call")
                    {
                        return None;
                    }
                    let payload: serde_json::Value = serde_json::from_str(&message.content).ok()?;
                    payload
                        .get("task_id")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                })
                .next();
            if let Some(task_id) = latest_async_task_id {
                if !request.messages.iter().any(|message| {
                    message.role == vv_agent::MessageRole::Tool
                        && message.tool_call_id.as_deref() == Some("parent_async_status")
                }) {
                    return Ok(LLMResponse::with_tool_calls(
                        "",
                        vec![ToolCall::new(
                            "parent_async_status",
                            "sub_task_status",
                            BTreeMap::from([
                                ("task_ids".to_string(), json!([task_id])),
                                ("detail_level".to_string(), json!("snapshot")),
                            ]),
                        )],
                    ));
                }
            }
        }

        let mut latest_status_payload = None;
        for message in &request.messages {
            if message.role == vv_agent::MessageRole::Tool
                && message.tool_call_id.as_deref() == Some("parent_async_status")
            {
                if let Ok(payload) = serde_json::from_str::<serde_json::Value>(&message.content) {
                    self.status_payloads
                        .lock()
                        .expect("status payloads poisoned")
                        .push(payload.clone());
                    latest_status_payload = Some(payload);
                }
            }
        }
        if let Some(payload) = latest_status_payload {
            let completed = payload["tasks"]
                .as_array()
                .and_then(|tasks| tasks.first())
                .is_some_and(|task| task["status"] == "completed");
            if completed {
                return Ok(LLMResponse::with_tool_calls(
                    "",
                    vec![ToolCall::new(
                        "parent_finish",
                        "task_finish",
                        BTreeMap::from([(
                            "message".to_string(),
                            json!("parent saw async child result"),
                        )]),
                    )],
                ));
            }
            if let Some(task_id) = payload["tasks"]
                .as_array()
                .and_then(|tasks| tasks.first())
                .and_then(|task| task["task_id"].as_str())
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
                return Ok(LLMResponse::with_tool_calls(
                    "",
                    vec![ToolCall::new(
                        "parent_async_status",
                        "sub_task_status",
                        BTreeMap::from([
                            ("task_ids".to_string(), json!([task_id])),
                            ("detail_level".to_string(), json!("snapshot")),
                        ]),
                    )],
                ));
            }
        }

        self.responses
            .lock()
            .map_err(|_| LlmError::Request("inspector poisoned".to_string()))?
            .pop_front()
            .ok_or(LlmError::ScriptExhausted)
    }
}

#[derive(Clone)]
struct InspectingSubTaskContinuationLlmClient {
    responses: Arc<Mutex<VecDeque<LLMResponse>>>,
    status_payloads: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl InspectingSubTaskContinuationLlmClient {
    fn new(responses: Vec<LLMResponse>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(VecDeque::from(responses))),
            status_payloads: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn status_payloads(&self) -> Vec<serde_json::Value> {
        self.status_payloads
            .lock()
            .expect("status payloads poisoned")
            .clone()
    }
}

impl LlmClient for InspectingSubTaskContinuationLlmClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        let is_child_request = request
            .messages
            .first()
            .is_some_and(|message| message.content.contains("research profile"));
        if is_child_request {
            let is_follow_up = request.messages.iter().any(|message| {
                message.role == vv_agent::MessageRole::User
                    && message.content.contains("Add appendix")
            });
            let message = if is_follow_up {
                "follow-up child complete"
            } else {
                "initial child complete"
            };
            return Ok(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::new(
                    if is_follow_up {
                        "child_follow_up_finish"
                    } else {
                        "child_initial_finish"
                    },
                    "task_finish",
                    BTreeMap::from([("message".to_string(), json!(message))]),
                )],
            ));
        }

        let latest_create_task_id = request
            .messages
            .iter()
            .rev()
            .filter_map(|message| {
                if message.role != vv_agent::MessageRole::Tool
                    || message.tool_call_id.as_deref() != Some("parent_async_sub_call")
                {
                    return None;
                }
                let payload: serde_json::Value = serde_json::from_str(&message.content).ok()?;
                payload
                    .get("task_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .next();

        if let Some(task_id) = latest_create_task_id {
            let mut latest_status_payload = None;
            let mut saw_continue_result = false;
            for message in &request.messages {
                if message.role != vv_agent::MessageRole::Tool {
                    continue;
                }
                if message.tool_call_id.as_deref() == Some("parent_async_status")
                    || message.tool_call_id.as_deref() == Some("parent_async_continue")
                {
                    if let Ok(payload) = serde_json::from_str::<serde_json::Value>(&message.content)
                    {
                        self.status_payloads
                            .lock()
                            .expect("status payloads poisoned")
                            .push(payload.clone());
                        if message.tool_call_id.as_deref() == Some("parent_async_continue") {
                            saw_continue_result = true;
                        }
                        latest_status_payload = Some(payload);
                    }
                }
            }

            if saw_continue_result {
                let follow_up_complete = latest_status_payload.as_ref().is_some_and(|payload| {
                    payload["tasks"][0]["status"] == "completed"
                        && payload["tasks"][0]["final_answer"] == "follow-up child complete"
                });
                return Ok(LLMResponse::with_tool_calls(
                    "",
                    vec![ToolCall::new(
                        "parent_finish",
                        "task_finish",
                        BTreeMap::from([(
                            "message".to_string(),
                            json!(if follow_up_complete {
                                "parent saw followed-up child result"
                            } else {
                                "parent saw follow-up failure"
                            }),
                        )]),
                    )],
                ));
            }

            if let Some(payload) = latest_status_payload {
                let completed = payload["tasks"]
                    .as_array()
                    .and_then(|tasks| tasks.first())
                    .is_some_and(|task| task["status"] == "completed");
                if completed {
                    return Ok(LLMResponse::with_tool_calls(
                        "",
                        vec![ToolCall::new(
                            "parent_async_continue",
                            "sub_task_status",
                            BTreeMap::from([
                                ("task_ids".to_string(), json!([task_id])),
                                ("detail_level".to_string(), json!("snapshot")),
                                ("message".to_string(), json!("Add appendix")),
                                ("wait_for_response".to_string(), json!(true)),
                            ]),
                        )],
                    ));
                }
            }

            std::thread::sleep(std::time::Duration::from_millis(10));
            return Ok(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::new(
                    "parent_async_status",
                    "sub_task_status",
                    BTreeMap::from([
                        ("task_ids".to_string(), json!([task_id])),
                        ("detail_level".to_string(), json!("snapshot")),
                    ]),
                )],
            ));
        }

        self.responses
            .lock()
            .map_err(|_| LlmError::Request("inspector poisoned".to_string()))?
            .pop_front()
            .ok_or(LlmError::ScriptExhausted)
    }
}

#[derive(Default)]
struct BeforeMemoryCompactHook {
    cycle_indexes: Mutex<Vec<u32>>,
}

impl BeforeMemoryCompactHook {
    fn cycle_indexes(&self) -> Vec<u32> {
        self.cycle_indexes
            .lock()
            .expect("cycle indexes poisoned")
            .clone()
    }
}

impl RuntimeHook for BeforeMemoryCompactHook {
    fn before_memory_compact(
        &self,
        event: vv_agent::BeforeMemoryCompactEvent<'_>,
    ) -> Option<Vec<Message>> {
        self.cycle_indexes
            .lock()
            .expect("cycle indexes poisoned")
            .push(event.cycle_index);
        assert_eq!(event.task.task_id, "pre_compact_hook_task");
        assert!(event.shared_state.contains_key("todo_list"));
        if event.cycle_index != 2 {
            return None;
        }
        assert!(event
            .messages
            .iter()
            .flat_map(|message| message.tool_calls.iter())
            .any(|call| call
                .arguments
                .get("content")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|content| content.contains("tool output"))));
        let mut messages = event.messages.to_vec();
        messages.push(Message::user("hook-added-before-memory-compact"));
        Some(messages)
    }
}

struct PendingToolCallIdHook;

impl RuntimeHook for PendingToolCallIdHook {
    fn before_tool_call(
        &self,
        event: vv_agent::BeforeToolCallEvent<'_>,
    ) -> Option<BeforeToolCallPatch> {
        assert_eq!(event.call.id, "pending_hook_finish");
        let mut result = ToolExecutionResult::success(
            "pending",
            json!({"message": "finished by pending hook"}).to_string(),
        );
        result.directive = ToolDirective::Finish;
        result.metadata.insert(
            "final_message".to_string(),
            json!("finished by pending hook"),
        );
        Some(BeforeToolCallPatch {
            call: None,
            result: Some(result),
        })
    }
}

struct DirectResultBeforeToolHook;

impl RuntimeHook for DirectResultBeforeToolHook {
    fn before_tool_call(
        &self,
        event: vv_agent::BeforeToolCallEvent<'_>,
    ) -> Option<BeforeToolCallPatch> {
        assert_eq!(event.call.id, "direct_result_finish");
        let mut result = ToolExecutionResult::success(
            event.call.id.clone(),
            json!({"message": "finished by direct result hook"}).to_string(),
        );
        result.directive = ToolDirective::Finish;
        result.metadata.insert(
            "final_message".to_string(),
            json!("finished by direct result hook"),
        );
        Some(result.into())
    }
}

struct PatchCallBeforeToolHook;

impl RuntimeHook for PatchCallBeforeToolHook {
    fn before_tool_call(
        &self,
        event: vv_agent::BeforeToolCallEvent<'_>,
    ) -> Option<BeforeToolCallPatch> {
        assert_eq!(event.call.id, "patch_call_finish");
        let mut patched = event.call.clone();
        patched.arguments.insert(
            "message".to_string(),
            json!("finished by patched call hook"),
        );
        Some(patched.into())
    }
}

struct PatchedCallAndBlankFinishHook;

impl RuntimeHook for PatchedCallAndBlankFinishHook {
    fn before_tool_call(
        &self,
        event: vv_agent::BeforeToolCallEvent<'_>,
    ) -> Option<BeforeToolCallPatch> {
        let mut patched = event.call.clone();
        patched.id = "runtime_patched_call".to_string();
        let mut result = ToolExecutionResult::success(
            "",
            json!({"message": "finished by patched short circuit"}).to_string(),
        );
        result.directive = ToolDirective::Finish;
        result.metadata.insert(
            "final_message".to_string(),
            json!("finished by patched short circuit"),
        );
        Some(BeforeToolCallPatch {
            call: Some(patched),
            result: Some(result),
        })
    }
}

#[derive(Default)]
struct InspectingRuntimeHook {
    events: Mutex<Vec<&'static str>>,
}

impl InspectingRuntimeHook {
    fn events(&self) -> Vec<&'static str> {
        self.events.lock().expect("events poisoned").clone()
    }
}

impl RuntimeHook for InspectingRuntimeHook {
    fn before_llm(&self, event: vv_agent::BeforeLlmEvent<'_>) -> Option<BeforeLlmPatch> {
        assert_eq!(event.cycle_index, 1);
        assert_eq!(event.task.task_id, "hook_task");
        assert!(event.shared_state.contains_key("todo_list"));
        self.events
            .lock()
            .expect("events poisoned")
            .push("before_llm");
        Some(BeforeLlmPatch {
            messages: Some(vec![Message::user("hooked user request")]),
            tool_schemas: Some(Vec::new()),
        })
    }

    fn after_llm(&self, event: vv_agent::AfterLlmEvent<'_>) -> Option<LLMResponse> {
        assert_eq!(event.messages[0].content, "hooked user request");
        assert!(event.tool_schemas.is_empty());
        self.events
            .lock()
            .expect("events poisoned")
            .push("after_llm");
        Some(event.response.clone())
    }

    fn before_tool_call(
        &self,
        event: vv_agent::BeforeToolCallEvent<'_>,
    ) -> Option<BeforeToolCallPatch> {
        assert_eq!(event.call.name, "task_finish");
        assert_eq!(event.context.cycle_index, 1);
        self.events
            .lock()
            .expect("events poisoned")
            .push("before_tool_call");
        Some(BeforeToolCallPatch {
            call: None,
            result: Some(ToolExecutionResult::success(
                event.call.id.clone(),
                json!({"message": "short-circuited by hook"}).to_string(),
            )),
        })
    }

    fn after_tool_call(
        &self,
        event: vv_agent::AfterToolCallEvent<'_>,
    ) -> Option<ToolExecutionResult> {
        assert_eq!(event.call.id, "hook_finish");
        assert!(event.result.content.contains("short-circuited"));
        self.events
            .lock()
            .expect("events poisoned")
            .push("after_tool_call");
        let mut result = event.result.clone();
        result.directive = ToolDirective::Finish;
        result.content = json!({"message": "final answer patched by after_tool_call"}).to_string();
        result.metadata.insert(
            "final_message".to_string(),
            json!("final answer patched by after_tool_call"),
        );
        Some(result)
    }
}

#[derive(Clone, Default)]
struct HookInspectingLlmClient {
    saw_hooked_message: Arc<Mutex<bool>>,
    tool_schema_counts: Arc<Mutex<Vec<usize>>>,
}

impl HookInspectingLlmClient {
    fn saw_hooked_message(&self) -> bool {
        *self.saw_hooked_message.lock().expect("flag poisoned")
    }

    fn tool_schema_counts(&self) -> Vec<usize> {
        self.tool_schema_counts
            .lock()
            .expect("schema counts poisoned")
            .clone()
    }
}

impl LlmClient for HookInspectingLlmClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        if request
            .messages
            .iter()
            .any(|message| message.content == "hooked user request")
        {
            *self.saw_hooked_message.lock().expect("flag poisoned") = true;
        }
        self.tool_schema_counts
            .lock()
            .expect("schema counts poisoned")
            .push(request.tools.len());
        Ok(LLMResponse::with_tool_calls(
            "finish through hook",
            vec![ToolCall::new(
                "hook_finish",
                "task_finish",
                BTreeMap::from([("message".to_string(), json!("original finish"))]),
            )],
        ))
    }
}

#[derive(Clone)]
struct MemoryCompactionInspectingLlmClient {
    responses_seen: Arc<Mutex<usize>>,
    large_tool_payload: String,
    second_request_messages: Arc<Mutex<Vec<Message>>>,
}

impl MemoryCompactionInspectingLlmClient {
    fn new(large_tool_payload: String) -> Self {
        Self {
            responses_seen: Arc::new(Mutex::new(0)),
            large_tool_payload,
            second_request_messages: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn second_request_messages(&self) -> Vec<Message> {
        self.second_request_messages
            .lock()
            .expect("messages poisoned")
            .clone()
    }
}

impl LlmClient for MemoryCompactionInspectingLlmClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
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
        *self
            .second_request_messages
            .lock()
            .expect("messages poisoned") = request.messages.clone();
        Ok(LLMResponse::with_tool_calls(
            "finish",
            vec![ToolCall::new(
                "finish_after_compact",
                "task_finish",
                BTreeMap::from([("message".to_string(), json!("memory compacted"))]),
            )],
        ))
    }
}

#[derive(Clone, Default)]
struct PromptTokenCompactionInspectingLlmClient {
    responses_seen: Arc<Mutex<usize>>,
    second_request_messages: Arc<Mutex<Vec<Message>>>,
}

impl PromptTokenCompactionInspectingLlmClient {
    fn second_request_messages(&self) -> Vec<Message> {
        self.second_request_messages
            .lock()
            .expect("messages poisoned")
            .clone()
    }
}

impl LlmClient for PromptTokenCompactionInspectingLlmClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        let mut responses_seen = self
            .responses_seen
            .lock()
            .map_err(|_| LlmError::Request("counter poisoned".to_string()))?;
        *responses_seen += 1;
        if *responses_seen == 1 {
            let mut response = LLMResponse::new("continue after measuring prompt tokens");
            response.token_usage = TokenUsage {
                prompt_tokens: 101,
                total_tokens: 120,
                ..TokenUsage::default()
            };
            return Ok(response);
        }
        *self
            .second_request_messages
            .lock()
            .expect("messages poisoned") = request.messages.clone();
        Ok(LLMResponse::with_tool_calls(
            "finish",
            vec![ToolCall::new(
                "finish_after_prompt_usage_compaction",
                "task_finish",
                BTreeMap::from([("message".to_string(), json!("memory compacted"))]),
            )],
        ))
    }
}

#[derive(Clone)]
struct MicrocompactInspectingLlmClient {
    responses_seen: Arc<Mutex<usize>>,
    large_tool_payload: String,
    third_request_messages: Arc<Mutex<Vec<Message>>>,
}

impl MicrocompactInspectingLlmClient {
    fn new(large_tool_payload: String) -> Self {
        Self {
            responses_seen: Arc::new(Mutex::new(0)),
            large_tool_payload,
            third_request_messages: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn third_request_messages(&self) -> Vec<Message> {
        self.third_request_messages
            .lock()
            .expect("messages poisoned")
            .clone()
    }
}

impl LlmClient for MicrocompactInspectingLlmClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        let mut responses_seen = self
            .responses_seen
            .lock()
            .map_err(|_| LlmError::Request("counter poisoned".to_string()))?;
        *responses_seen += 1;
        if *responses_seen == 1 {
            return Ok(LLMResponse::with_tool_calls(
                "first cycle",
                vec![ToolCall::new(
                    "bash_large",
                    "bash",
                    BTreeMap::from([(
                        "command".to_string(),
                        json!(format!("printf '{}'", self.large_tool_payload)),
                    )]),
                )],
            ));
        }
        if *responses_seen == 2 {
            return Ok(LLMResponse::new(
                "continue once so older tool output can age",
            ));
        }
        *self
            .third_request_messages
            .lock()
            .expect("messages poisoned") = request.messages.clone();
        Ok(LLMResponse::with_tool_calls(
            "finish",
            vec![ToolCall::new(
                "finish_after_microcompact",
                "task_finish",
                BTreeMap::from([("message".to_string(), json!("memory compacted"))]),
            )],
        ))
    }
}

#[derive(Clone, Default)]
struct PromptTooLongRetryLlmClient {
    requests_seen: Arc<Mutex<usize>>,
    final_request_messages: Arc<Mutex<Vec<Message>>>,
}

impl PromptTooLongRetryLlmClient {
    fn request_count(&self) -> usize {
        *self.requests_seen.lock().expect("request count poisoned")
    }

    fn final_request_messages(&self) -> Vec<Message> {
        self.final_request_messages
            .lock()
            .expect("messages poisoned")
            .clone()
    }
}

impl LlmClient for PromptTooLongRetryLlmClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        let mut requests_seen = self
            .requests_seen
            .lock()
            .map_err(|_| LlmError::Request("request count poisoned".to_string()))?;
        *requests_seen += 1;
        if *requests_seen <= 2 {
            return Err(LlmError::Request(
                "Prompt is too long for this model".to_string(),
            ));
        }
        *self
            .final_request_messages
            .lock()
            .expect("messages poisoned") = request.messages;
        Ok(LLMResponse::new("done after retry"))
    }
}

#[derive(Clone, Default)]
struct AlwaysPromptTooLongLlmClient {
    requests_seen: Arc<Mutex<usize>>,
}

impl AlwaysPromptTooLongLlmClient {
    fn request_count(&self) -> usize {
        *self.requests_seen.lock().expect("request count poisoned")
    }
}

impl LlmClient for AlwaysPromptTooLongLlmClient {
    fn complete(&self, _request: LlmRequest) -> Result<LLMResponse, LlmError> {
        let mut requests_seen = self
            .requests_seen
            .lock()
            .map_err(|_| LlmError::Request("request count poisoned".to_string()))?;
        *requests_seen += 1;
        Err(LlmError::Request(
            "Prompt is too long for this model".to_string(),
        ))
    }
}

#[derive(Clone, Default)]
struct StreamingSubAgentLlmClient {
    calls_seen: Arc<Mutex<usize>>,
}

impl LlmClient for StreamingSubAgentLlmClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        self.complete_with_stream(request, None)
    }

    fn complete_with_stream(
        &self,
        _request: LlmRequest,
        stream_callback: Option<LlmStreamCallback>,
    ) -> Result<LLMResponse, LlmError> {
        let mut calls_seen = self
            .calls_seen
            .lock()
            .map_err(|_| LlmError::Request("call counter poisoned".to_string()))?;
        *calls_seen += 1;
        match *calls_seen {
            1 => Ok(LLMResponse::with_tool_calls(
                "delegate",
                vec![ToolCall::new(
                    "parent_sub_call",
                    "create_sub_task",
                    BTreeMap::from([
                        ("agent_id".to_string(), json!("researcher")),
                        ("task_description".to_string(), json!("Collect core facts")),
                    ]),
                )],
            )),
            2 => {
                if let Some(callback) = stream_callback {
                    callback(&BTreeMap::from([
                        ("event".to_string(), json!("assistant_delta")),
                        ("content_delta".to_string(), json!("checking")),
                    ]));
                    callback(&BTreeMap::from([
                        ("event".to_string(), json!("tool_call_started")),
                        ("tool_call_id".to_string(), json!("sub_tool_1")),
                        ("tool_call_index".to_string(), json!(0)),
                        ("function_name".to_string(), json!("bash")),
                        ("arguments_chars".to_string(), json!(0)),
                        ("estimated_tokens".to_string(), json!(0)),
                    ]));
                    callback(&BTreeMap::from([
                        ("event".to_string(), json!("tool_call_progress")),
                        ("tool_call_id".to_string(), json!("sub_tool_1")),
                        ("tool_call_index".to_string(), json!(0)),
                        ("function_name".to_string(), json!("bash")),
                        ("arguments_chars".to_string(), json!(48)),
                        ("estimated_tokens".to_string(), json!(12)),
                    ]));
                }
                Ok(LLMResponse::with_tool_calls(
                    "sub finish",
                    vec![ToolCall::new(
                        "sub_finish",
                        "task_finish",
                        BTreeMap::from([("message".to_string(), json!("sub done"))]),
                    )],
                ))
            }
            _ => Ok(LLMResponse::with_tool_calls(
                "parent finish",
                vec![ToolCall::new(
                    "parent_finish",
                    "task_finish",
                    BTreeMap::from([("message".to_string(), json!("parent done"))]),
                )],
            )),
        }
    }
}

#[derive(Clone)]
struct SessionMemoryExtractingLlmClient {
    responses_seen: Arc<Mutex<usize>>,
    extraction_prompt_count: Arc<Mutex<usize>>,
    large_tool_payload: String,
    second_request_messages: Arc<Mutex<Vec<Message>>>,
}

impl SessionMemoryExtractingLlmClient {
    fn new(large_tool_payload: String) -> Self {
        Self {
            responses_seen: Arc::new(Mutex::new(0)),
            extraction_prompt_count: Arc::new(Mutex::new(0)),
            large_tool_payload,
            second_request_messages: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn extraction_prompt_count(&self) -> usize {
        *self
            .extraction_prompt_count
            .lock()
            .expect("extraction count poisoned")
    }

    fn second_request_messages(&self) -> Vec<Message> {
        self.second_request_messages
            .lock()
            .expect("messages poisoned")
            .clone()
    }
}

impl LlmClient for SessionMemoryExtractingLlmClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        if request.tools.is_empty()
            && request.messages.len() == 1
            && request.messages[0]
                .content
                .contains("extract durable facts that should survive context compression")
        {
            *self
                .extraction_prompt_count
                .lock()
                .expect("extraction count poisoned") += 1;
            return Ok(LLMResponse::new(
                r#"[{"category":"key_fact","content":"default callback preserved this fact","importance":9}]"#,
            ));
        }

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
        *self
            .second_request_messages
            .lock()
            .expect("messages poisoned") = request.messages.clone();
        Ok(LLMResponse::with_tool_calls(
            "finish",
            vec![ToolCall::new(
                "finish_after_compact",
                "task_finish",
                BTreeMap::from([("message".to_string(), json!("memory compacted"))]),
            )],
        ))
    }
}

#[derive(Clone, Default)]
struct DefaultSessionMemoryInspectingLlmClient {
    first_request_messages: Arc<Mutex<Vec<Message>>>,
}

impl DefaultSessionMemoryInspectingLlmClient {
    fn first_request_messages(&self) -> Vec<Message> {
        self.first_request_messages
            .lock()
            .expect("messages poisoned")
            .clone()
    }
}

impl LlmClient for DefaultSessionMemoryInspectingLlmClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        let mut first_request = self
            .first_request_messages
            .lock()
            .map_err(|_| LlmError::Request("messages poisoned".to_string()))?;
        if first_request.is_empty() {
            *first_request = request.messages;
        }
        Ok(LLMResponse::new("done"))
    }
}

#[derive(Clone, Default)]
struct SummaryModelInspectingLlmClient {
    extraction_model: Arc<Mutex<Option<String>>>,
}

impl SummaryModelInspectingLlmClient {
    fn extraction_model(&self) -> Option<String> {
        self.extraction_model
            .lock()
            .expect("model poisoned")
            .clone()
    }
}

impl LlmClient for SummaryModelInspectingLlmClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        if request.tools.is_empty()
            && request.messages.len() == 1
            && request.messages[0]
                .content
                .contains("extract durable facts that should survive context compression")
        {
            *self
                .extraction_model
                .lock()
                .map_err(|_| LlmError::Request("model poisoned".to_string()))? =
                Some(request.model.clone());
            return Ok(LLMResponse::new(
                r#"[{"category":"key_fact","content":"summary model captured","importance":8}]"#,
            ));
        }
        Ok(LLMResponse::new("done"))
    }
}
