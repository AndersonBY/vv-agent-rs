use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use vv_agent::{
    create_agent_session, create_agent_session_with_shared_state, AgentDefinition, AgentRun,
    AgentRuntime, AgentSDKClient, AgentSDKOptions, AgentSession, AgentStatus, BeforeLlmEvent,
    BeforeToolCallEvent, BeforeToolCallPatch, CycleRecord, LLMResponse, LlmClient, LlmError,
    LlmRequest, MessageRole, ResolvedModelConfig, RuntimeHook, ScriptedLlmClient,
    SessionEventHandler, SubAgentConfig, TokenUsage, ToolCall, ToolDirective, ToolExecutionResult,
};

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

#[path = "sdk_session/runtime_controls.rs"]
mod runtime_controls;
#[path = "sdk_session/state_queue.rs"]
mod state_queue;
#[path = "sdk_session/wait_user.rs"]
mod wait_user;
#[path = "sdk_session/workspace_sessions.rs"]
mod workspace_sessions;

fn fake_run(prompt: &str, status: AgentStatus) -> AgentRun {
    let mut result = vv_agent::AgentResult::completed(vec![], vec![], prompt.to_string());
    result.status = status;
    if status == AgentStatus::WaitUser {
        result.wait_reason = Some("need input".to_string());
        result.final_answer = None;
    }
    AgentRun {
        agent_name: "demo".to_string(),
        result,
        resolved: ResolvedModelConfig::new("demo", "demo", "demo", "demo", vec![]),
    }
}

type RecordedEvents = Arc<Mutex<Vec<(String, BTreeMap<String, Value>)>>>;

struct ShellMetadataCaptureHook {
    captured_metadata: Arc<Mutex<Vec<BTreeMap<String, Value>>>>,
}

struct TaskMetadataCaptureHook {
    captured_metadata: Arc<Mutex<Vec<BTreeMap<String, Value>>>>,
}

impl RuntimeHook for TaskMetadataCaptureHook {
    fn before_llm(&self, event: BeforeLlmEvent<'_>) -> Option<vv_agent::BeforeLlmPatch> {
        self.captured_metadata
            .lock()
            .expect("captured metadata")
            .push(event.task.metadata.clone());
        None
    }
}

impl RuntimeHook for ShellMetadataCaptureHook {
    fn before_tool_call(&self, event: BeforeToolCallEvent<'_>) -> Option<BeforeToolCallPatch> {
        if event.call.name != "bash" {
            return None;
        }
        self.captured_metadata
            .lock()
            .expect("captured metadata")
            .push(event.context.metadata.clone());
        let mut result = ToolExecutionResult::success(event.call.id.clone(), "{}");
        result.directive = ToolDirective::Continue;
        Some(BeforeToolCallPatch {
            call: None,
            result: Some(result),
        })
    }
}

fn recorded_events() -> RecordedEvents {
    Arc::new(Mutex::new(Vec::new()))
}

fn recording_listener(events: &RecordedEvents) -> SessionEventHandler {
    let events = Arc::clone(events);
    Arc::new(move |event, payload| {
        events
            .lock()
            .expect("events")
            .push((event.to_string(), payload.clone()));
    })
}

#[derive(Debug)]
struct RuntimeSnapshot {
    messages: Vec<String>,
    shared_state: BTreeMap<String, Value>,
}

struct RecordingRuntimeHook {
    snapshots: Arc<Mutex<Vec<RuntimeSnapshot>>>,
}

impl RuntimeHook for RecordingRuntimeHook {
    fn before_llm(&self, event: BeforeLlmEvent<'_>) -> Option<vv_agent::BeforeLlmPatch> {
        self.snapshots
            .lock()
            .expect("snapshots")
            .push(RuntimeSnapshot {
                messages: event
                    .messages
                    .iter()
                    .map(|message| {
                        format!("{:?}:{}", message.role, message.content).to_ascii_lowercase()
                    })
                    .collect(),
                shared_state: event.shared_state.clone(),
            });
        None
    }
}

struct WorkspaceRecordingHook {
    workspaces: Arc<Mutex<Vec<std::path::PathBuf>>>,
}

impl RuntimeHook for WorkspaceRecordingHook {
    fn before_tool_call(&self, event: BeforeToolCallEvent<'_>) -> Option<BeforeToolCallPatch> {
        self.workspaces
            .lock()
            .expect("workspaces")
            .push(event.context.workspace.clone());
        None
    }
}

#[derive(Clone, Default)]
struct SessionSubTaskManagerLlm;

impl LlmClient for SessionSubTaskManagerLlm {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        let is_child_request = request.messages.iter().any(|message| {
            message.role == MessageRole::User && message.content.contains("collect session facts")
        });
        if is_child_request {
            return Ok(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::new(
                    "child_finish",
                    "task_finish",
                    json_args(serde_json::json!({"message": "child complete"})),
                )],
            ));
        }

        let latest_user = request
            .messages
            .iter()
            .rev()
            .find(|message| message.role == MessageRole::User)
            .map(|message| message.content.as_str())
            .unwrap_or_default();
        let latest_task_id = request.messages.iter().rev().find_map(|message| {
            if message.role != MessageRole::Tool
                || message.tool_call_id.as_deref() != Some("session_sub_create")
            {
                return None;
            }
            let payload = serde_json::from_str::<Value>(&message.content).ok()?;
            payload
                .get("task_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        });

        if let Some(status_payload) = request.messages.iter().rev().find_map(|message| {
            if message.role != MessageRole::Tool
                || message.tool_call_id.as_deref() != Some("session_sub_status")
            {
                return None;
            }
            serde_json::from_str::<Value>(&message.content).ok()
        }) {
            let found = status_payload["tasks"]
                .as_array()
                .and_then(|tasks| tasks.first())
                .is_some_and(|task| task.get("error").is_none());
            return Ok(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::new(
                    "session_finish_after_status",
                    "task_finish",
                    json_args(serde_json::json!({
                        "message": if found { "found prior child" } else { "lost prior child" }
                    })),
                )],
            ));
        }

        if latest_user.contains("check prior child") {
            let task_id = latest_task_id.unwrap_or_else(|| "missing-task-id".to_string());
            return Ok(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::new(
                    "session_sub_status",
                    "sub_task_status",
                    json_args(serde_json::json!({
                        "task_ids": [task_id],
                        "detail_level": "snapshot"
                    })),
                )],
            ));
        }

        if latest_task_id.is_some() {
            return Ok(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::new(
                    "session_finish_after_create",
                    "task_finish",
                    json_args(serde_json::json!({"message": "created child"})),
                )],
            ));
        }

        Ok(LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::new(
                "session_sub_create",
                "create_sub_task",
                json_args(serde_json::json!({
                    "agent_id": "researcher",
                    "task_description": "collect session facts",
                    "wait_for_completion": false
                })),
            )],
        ))
    }
}

fn json_args(value: Value) -> BTreeMap<String, Value> {
    value
        .as_object()
        .expect("object args")
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}
