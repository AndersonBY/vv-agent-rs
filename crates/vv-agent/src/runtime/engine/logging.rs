use std::collections::BTreeMap;

use serde_json::Value;

use crate::llm::LlmClient;
use std::path::Path;

use crate::types::{
    AgentResult, AgentTask, CycleRecord, ToolCall, ToolExecutionResult, ToolResultStatus,
};

use super::{AgentRuntime, RuntimeEventHandler, RuntimeLogHandler, RuntimeRunControls};

pub(super) fn emit_runtime_log(
    runtime_handler: Option<&RuntimeLogHandler>,
    event_handler: Option<&RuntimeEventHandler>,
    execution_context: Option<&crate::runtime::context::ExecutionContext>,
    event: &str,
    mut payload: BTreeMap<String, Value>,
) {
    if let Some(context) = execution_context {
        for (metadata_key, payload_key) in [
            ("_vv_agent_run_id", "run_id"),
            ("_vv_agent_trace_id", "trace_id"),
            ("_vv_agent_agent_name", "agent_name"),
            ("_vv_agent_session_id", "session_id"),
        ] {
            if let Some(value) = context.metadata.get(metadata_key) {
                payload.insert(payload_key.to_string(), value.clone());
            }
        }
    }
    if let Some(handler) = runtime_handler {
        if let Ok(mut handler) = handler.lock() {
            (handler)(event, &payload);
        }
    }
    if let Some(handler) = event_handler {
        handler(event, &payload);
    }
}

impl<C: LlmClient> AgentRuntime<C> {
    pub(super) fn emit_log(
        &self,
        controls: &RuntimeRunControls,
        event: &str,
        payload: BTreeMap<String, Value>,
    ) {
        emit_runtime_log(
            self.log_handler.as_ref(),
            controls.log_handler.as_ref(),
            controls.execution_context.as_ref(),
            event,
            payload,
        );
    }

    pub(super) fn emit_cycle_llm_response(
        &self,
        controls: &RuntimeRunControls,
        cycle: &CycleRecord,
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
                    serde_json::to_value(&cycle.token_usage).unwrap_or(Value::Null),
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
