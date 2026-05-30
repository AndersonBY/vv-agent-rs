use super::*;

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
fn runtime_collects_cycle_and_total_token_usage_from_llm_responses() {
    let todo_args = BTreeMap::from([(
        "todos".to_string(),
        json!([
            {"title": "draft", "status": "completed", "priority": "medium"}
        ]),
    )]);
    let mut planning_response = LLMResponse::with_tool_calls(
        "planning",
        vec![ToolCall::new("todo_call", "todo_write", todo_args)],
    );
    planning_response.raw.insert(
        "usage".to_string(),
        json!({
            "prompt_tokens": 100,
            "completion_tokens": 25,
            "total_tokens": 125,
            "prompt_tokens_details": {"cached_tokens": 40},
            "completion_tokens_details": {"reasoning_tokens": 10}
        }),
    );

    let finish_args = BTreeMap::from([("message".to_string(), json!("ok"))]);
    let mut finish_response = LLMResponse::with_tool_calls(
        "done",
        vec![ToolCall::new("finish_call", "task_finish", finish_args)],
    );
    finish_response.raw.insert(
        "usage".to_string(),
        json!({
            "input_tokens": 50,
            "output_tokens": 30,
            "total_tokens": 80,
            "input_tokens_details": {"cache_creation_tokens": 12}
        }),
    );

    let runtime = AgentRuntime::new(ScriptedLlmClient::new(vec![
        planning_response,
        finish_response,
    ]));
    let mut task = AgentTask::new("task_usage", "demo", "system", "go");
    task.max_cycles = 4;

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(result.token_usage.cycles.len(), 2);
    assert_eq!(result.cycles[0].token_usage.prompt_tokens, 100);
    assert_eq!(result.cycles[0].token_usage.completion_tokens, 25);
    assert_eq!(result.cycles[0].token_usage.cached_tokens, 40);
    assert_eq!(result.cycles[0].token_usage.reasoning_tokens, 10);
    assert_eq!(result.cycles[1].token_usage.prompt_tokens, 50);
    assert_eq!(result.cycles[1].token_usage.completion_tokens, 30);
    assert_eq!(result.cycles[1].token_usage.cache_creation_tokens, 12);

    assert_eq!(result.token_usage.prompt_tokens, 150);
    assert_eq!(result.token_usage.completion_tokens, 55);
    assert_eq!(result.token_usage.total_tokens, 205);
    assert_eq!(result.token_usage.cached_tokens, 40);
    assert_eq!(result.token_usage.reasoning_tokens, 10);
    assert_eq!(result.token_usage.cache_creation_tokens, 12);
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
