use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use serde_json::json;
use vv_agent::{
    AgentRuntime, AgentStatus, AgentTask, LLMResponse, LlmClient, LlmError, LlmRequest,
    ScriptedLlmClient, ToolCall, ToolDirective,
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
