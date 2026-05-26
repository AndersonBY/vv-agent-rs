use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use serde_json::json;
use vv_agent::{
    memory::CLEARED_MARKER, AgentRuntime, AgentStatus, AgentTask, BeforeLlmPatch,
    BeforeToolCallPatch, LLMResponse, LlmClient, LlmError, LlmRequest, Message, RuntimeHook,
    ScriptedLlmClient, SubAgentConfig, ToolCall, ToolDirective, ToolExecutionResult,
};

const PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x60, 0x00, 0x00, 0x00,
    0x02, 0x00, 0x01, 0xe2, 0x21, 0xbc, 0x33, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];

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
fn runtime_executes_configured_sub_agent_with_real_runner() {
    let mut sub_task_args = BTreeMap::new();
    sub_task_args.insert("agent_id".to_string(), json!("researcher"));
    sub_task_args.insert(
        "task_description".to_string(),
        json!("Find the migration target"),
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
fn runtime_can_poll_async_configured_sub_agent_status() {
    let mut sub_task_args = BTreeMap::new();
    sub_task_args.insert("agent_id".to_string(), json!("researcher"));
    sub_task_args.insert(
        "task_description".to_string(),
        json!("Collect async migration facts"),
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
fn runtime_emits_reference_lifecycle_log_events() {
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
    assert_eq!(inspector.extraction_prompt_count(), 1);
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
            .is_some_and(|message| message.content == "research profile");
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
        assert_eq!(event.cycle_index, 0);
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
        assert_eq!(event.context.cycle_index, 0);
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
