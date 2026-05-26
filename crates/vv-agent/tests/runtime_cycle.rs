use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use serde_json::json;
use vv_agent::{
    AgentRuntime, AgentStatus, AgentTask, LLMResponse, LlmClient, LlmError, LlmRequest,
    ScriptedLlmClient, SubAgentConfig, ToolCall, ToolDirective,
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
