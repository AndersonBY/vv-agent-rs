use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};

use serde_json::Value;

use crate::events::{DiagnosticLevel, RunEvent};
use crate::llm::LlmClient;
use std::path::Path;

use crate::types::{
    AgentResult, AgentTask, CycleRecord, ToolCall, ToolExecutionResult, ToolResultStatus,
};

use super::{AgentRuntime, RunEventHandler, RuntimeRunControls};

pub(super) fn emit_runtime_event(
    runtime_handler: Option<&RunEventHandler>,
    run_handler: Option<&RunEventHandler>,
    execution_context: Option<&crate::runtime::context::ExecutionContext>,
    code: &str,
    payload: BTreeMap<String, Value>,
) {
    let metadata = execution_context
        .map(|context| &context.metadata)
        .cloned()
        .unwrap_or_default();
    let run_id = identity(&metadata, &["_vv_agent_run_id", "run_id"])
        .unwrap_or_else(|| "runtime".to_string());
    let trace_id =
        identity(&metadata, &["_vv_agent_trace_id", "trace_id"]).unwrap_or_else(|| run_id.clone());
    let agent_name = identity(&metadata, &["_vv_agent_agent_name", "agent_name"])
        .unwrap_or_else(|| "runtime".to_string());
    let session_id = identity(&metadata, &["_vv_agent_session_id", "session_id"]);
    let parent_run_id = identity(&metadata, &["_vv_agent_parent_run_id", "parent_run_id"]);
    let input = identity(&metadata, &["_vv_agent_input"]).unwrap_or_default();
    let context = crate::runner::RuntimeEventContext::new(
        &run_id,
        &trace_id,
        &agent_name,
        session_id.clone(),
        input,
    );
    let terminal_observation = matches!(
        code,
        "cycle_failed"
            | "run_cancelled"
            | "run_completed"
            | "run_failed"
            | "run_max_cycles"
            | "run_wait_user"
    );
    let mut event = if terminal_observation {
        None
    } else {
        crate::runner::map_runtime_event(code, &payload, &context)
    }
    .unwrap_or_else(|| diagnostic_event(&run_id, &trace_id, &agent_name, code, payload));
    if event.session_id().is_none() {
        if let Some(session_id) = session_id {
            event = event.with_session_id(session_id);
        }
    }
    if event.parent_run_id().is_none() {
        if let Some(parent_run_id) = parent_run_id {
            event = event.with_parent_run_id(parent_run_id);
        }
    }

    let handler = execution_context
        .and_then(|context| context.event_handler.as_ref())
        .or(run_handler)
        .or(runtime_handler);
    if let Some(handler) = handler {
        let _ = catch_unwind(AssertUnwindSafe(|| handler(&event)));
    }
}

fn diagnostic_event(
    run_id: &str,
    trace_id: &str,
    agent_name: &str,
    code: &str,
    mut payload: BTreeMap<String, Value>,
) -> RunEvent {
    let cycle_index = payload
        .remove("cycle")
        .or_else(|| payload.remove("cycle_index"))
        .and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0);
    for field in [
        "run_id",
        "trace_id",
        "session_id",
        "parent_run_id",
        "agent_name",
    ] {
        payload.remove(field);
    }
    RunEvent::diagnostic(
        run_id,
        trace_id,
        agent_name,
        cycle_index,
        diagnostic_level(code),
        code,
        payload.into_iter().collect(),
    )
}

fn diagnostic_level(code: &str) -> DiagnosticLevel {
    if code.ends_with("_failed") || code == "cycle_failed" {
        DiagnosticLevel::Error
    } else if code == "run_max_cycles" {
        DiagnosticLevel::Warning
    } else if code.starts_with("after_cycle_") || matches!(code, "run_steered" | "run_wait_user") {
        DiagnosticLevel::Info
    } else {
        DiagnosticLevel::Debug
    }
}

fn identity(metadata: &BTreeMap<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        metadata
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

impl<C: LlmClient> AgentRuntime<C> {
    pub(super) fn emit_log(
        &self,
        controls: &RuntimeRunControls,
        event: &str,
        payload: BTreeMap<String, Value>,
    ) {
        emit_runtime_event(
            self.event_handler.as_ref(),
            controls.event_handler.as_ref(),
            controls.execution_context.as_ref(),
            event,
            payload,
        );
    }

    pub(super) fn emit_cycle_llm_response(
        &self,
        controls: &RuntimeRunControls,
        cycle: &CycleRecord,
        token_usage: &crate::types::TokenUsage,
    ) {
        self.emit_log(
            controls,
            "cycle_llm_response",
            BTreeMap::from([
                ("cycle".to_string(), Value::from(cycle.index)),
                (
                    "assistant_message".to_string(),
                    Value::String(cycle.assistant_message.clone()),
                ),
                (
                    "assistant_preview".to_string(),
                    Value::String(self.preview_text(&cycle.assistant_message)),
                ),
                (
                    "tool_calls".to_string(),
                    serde_json::to_value(&cycle.tool_calls).unwrap_or(Value::Null),
                ),
                (
                    "tool_call_names".to_string(),
                    Value::Array(
                        cycle
                            .tool_calls
                            .iter()
                            .map(|call| Value::String(call.name.clone()))
                            .collect(),
                    ),
                ),
                (
                    "tool_call_count".to_string(),
                    Value::from(cycle.tool_calls.len()),
                ),
                (
                    "memory_compacted".to_string(),
                    Value::Bool(cycle.memory_compacted),
                ),
                (
                    "token_usage".to_string(),
                    serde_json::to_value(token_usage).unwrap_or(Value::Null),
                ),
            ]),
        );
    }

    pub(super) fn emit_run_started(
        &self,
        controls: &RuntimeRunControls,
        task: &AgentTask,
        workspace_path: &Path,
    ) {
        self.emit_log(
            controls,
            "run_started",
            BTreeMap::from([
                ("task_id".to_string(), Value::String(task.task_id.clone())),
                (
                    "agent_name".to_string(),
                    Value::String(
                        task.metadata
                            .get("_vv_agent_agent_name")
                            .or_else(|| task.metadata.get("agent_name"))
                            .and_then(Value::as_str)
                            .unwrap_or(&task.task_id)
                            .to_string(),
                    ),
                ),
                ("input".to_string(), Value::String(task.user_prompt.clone())),
                ("model".to_string(), Value::String(task.model.clone())),
                (
                    "workspace".to_string(),
                    Value::String(workspace_path.display().to_string()),
                ),
                ("max_cycles".to_string(), Value::from(task.max_cycles)),
            ]),
        );
    }

    pub(super) fn emit_run_max_cycles(&self, controls: &RuntimeRunControls, result: &AgentResult) {
        self.emit_log(
            controls,
            "run_max_cycles",
            BTreeMap::from([
                ("cycle".to_string(), Value::from(result.cycles.len())),
                (
                    "final_answer".to_string(),
                    Value::String(
                        self.preview_text(&result.final_answer.clone().unwrap_or_default()),
                    ),
                ),
                (
                    "error".to_string(),
                    Value::String(self.preview_text(&result.error.clone().unwrap_or_default())),
                ),
                (
                    "completion_reason".to_string(),
                    serde_json::to_value(result.completion_reason).unwrap_or(Value::Null),
                ),
                (
                    "partial_output".to_string(),
                    Value::String(
                        self.preview_text(&result.partial_output.clone().unwrap_or_default()),
                    ),
                ),
            ]),
        );
    }

    pub(super) fn emit_tool_result(
        &self,
        controls: &RuntimeRunControls,
        cycle_index: u32,
        call: &ToolCall,
        result: &ToolExecutionResult,
    ) {
        self.emit_tool_result_with_lifecycle(controls, cycle_index, call, result, false);
    }

    pub(super) fn emit_skipped_tool_result(
        &self,
        controls: &RuntimeRunControls,
        cycle_index: u32,
        call: &ToolCall,
        result: &ToolExecutionResult,
    ) {
        self.emit_tool_result_with_lifecycle(controls, cycle_index, call, result, true);
    }

    fn emit_tool_result_with_lifecycle(
        &self,
        controls: &RuntimeRunControls,
        cycle_index: u32,
        call: &ToolCall,
        result: &ToolExecutionResult,
        lifecycle_suppressed: bool,
    ) {
        let mut payload = BTreeMap::from([
            ("cycle".to_string(), Value::from(cycle_index)),
            ("tool_name".to_string(), Value::String(call.name.clone())),
            (
                "tool_arguments".to_string(),
                Value::Object(call.arguments.clone().into_iter().collect()),
            ),
            (
                "tool_call_id".to_string(),
                Value::String(result.tool_call_id.clone()),
            ),
            (
                "status".to_string(),
                tool_result_status_value(result.status),
            ),
            (
                "directive".to_string(),
                serde_json::to_value(result.directive).unwrap_or(Value::Null),
            ),
            (
                "error_code".to_string(),
                result
                    .error_code
                    .clone()
                    .map(Value::String)
                    .unwrap_or(Value::Null),
            ),
            ("content".to_string(), Value::String(result.content.clone())),
            (
                "content_preview".to_string(),
                Value::String(self.preview_text(&result.content)),
            ),
            (
                "metadata".to_string(),
                Value::Object(result.metadata.clone().into_iter().collect()),
            ),
        ]);
        if lifecycle_suppressed {
            payload.insert("lifecycle_suppressed".to_string(), Value::Bool(true));
        }
        self.emit_log(controls, "tool_result", payload);
    }

    pub(super) fn preview_text(&self, text: &str) -> String {
        let cleaned = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let Some(limit) = self.log_preview_chars.map(|limit| limit.max(40)) else {
            return cleaned;
        };
        if cleaned.chars().count() <= limit {
            return cleaned;
        }
        let prefix = cleaned
            .chars()
            .take(limit.saturating_sub(3))
            .collect::<String>();
        format!("{prefix}...")
    }
}

pub(super) fn tool_result_status_value(status: ToolResultStatus) -> Value {
    let status = match status {
        ToolResultStatus::Success => "success",
        ToolResultStatus::Error => "error",
        ToolResultStatus::WaitResponse => "wait_response",
        ToolResultStatus::Running => "running",
        ToolResultStatus::PendingCompress => "pending_compress",
    };
    Value::String(status.to_string())
}
