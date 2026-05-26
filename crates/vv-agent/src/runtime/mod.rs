pub mod hooks;
mod results;
mod sub_agents;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::llm::{LlmClient, LlmError, LlmRequest};
use crate::memory::{MemoryManager, MemoryManagerConfig};
use crate::sub_task_manager::SubTaskManager;
use crate::tools::{build_default_registry, ToolContext, ToolRegistry};
use crate::types::{
    AgentResult, AgentStatus, AgentTask, ToolCall, ToolDirective, ToolExecutionResult,
    ToolResultStatus,
};
use crate::workspace::{LocalWorkspaceBackend, WorkspaceBackend};

pub use hooks::{
    AfterLlmEvent, AfterToolCallEvent, BeforeLlmEvent, BeforeLlmPatch, BeforeToolCallEvent,
    BeforeToolCallPatch, RuntimeHook, RuntimeHookManager,
};
use results::{assistant_message_from_response, extract_final_message, extract_wait_reason};

pub type RuntimeLogCallback = dyn FnMut(&str, &BTreeMap<String, Value>) + Send + Sync + 'static;
pub type RuntimeLogHandler = Arc<Mutex<Box<RuntimeLogCallback>>>;

#[derive(Debug, Clone, Default)]
pub struct CancellationToken {
    cancelled: bool,
}

impl CancellationToken {
    pub fn cancel(&mut self) {
        self.cancelled = true;
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }
}

pub struct AgentRuntime<C: LlmClient> {
    pub llm_client: C,
    pub tool_registry: ToolRegistry,
    pub default_workspace: Option<PathBuf>,
    pub log_handler: Option<RuntimeLogHandler>,
    pub workspace_backend: Arc<dyn WorkspaceBackend>,
    pub hooks: Vec<Arc<dyn RuntimeHook>>,
}

impl<C: LlmClient> AgentRuntime<C> {
    pub fn new(llm_client: C) -> Self {
        Self {
            llm_client,
            tool_registry: build_default_registry(),
            default_workspace: None,
            log_handler: None,
            workspace_backend: Arc::new(LocalWorkspaceBackend::new(PathBuf::from("./workspace"))),
            hooks: Vec::new(),
        }
    }

    pub fn with_tool_registry(mut self, tool_registry: ToolRegistry) -> Self {
        self.tool_registry = tool_registry;
        self
    }
}

impl<C: LlmClient + Clone + 'static> AgentRuntime<C> {
    pub fn run(&self, task: AgentTask) -> Result<AgentResult, LlmError> {
        let mut messages = Vec::new();
        if !task.system_prompt.is_empty() {
            messages.push(crate::types::Message::system(task.system_prompt.clone()));
        }
        messages.push(crate::types::Message::user(task.user_prompt.clone()));

        let mut cycles = Vec::new();
        let mut shared_state = BTreeMap::new();
        shared_state.insert("todo_list".to_string(), Value::Array(Vec::new()));
        let workspace_path = self
            .default_workspace
            .clone()
            .unwrap_or_else(|| PathBuf::from("./workspace"));
        let sub_task_manager = SubTaskManager::default();
        self.emit_log(
            "run_started",
            BTreeMap::from([
                ("task_id".to_string(), Value::String(task.task_id.clone())),
                ("model".to_string(), Value::String(task.model.clone())),
                (
                    "workspace".to_string(),
                    Value::String(workspace_path.display().to_string()),
                ),
                ("max_cycles".to_string(), Value::from(task.max_cycles)),
            ]),
        );

        let memory_manager = build_memory_manager(&task, workspace_path.clone());

        for cycle_index in 0..task.max_cycles {
            self.emit_log(
                "cycle_started",
                BTreeMap::from([
                    ("cycle".to_string(), Value::from(cycle_index)),
                    ("max_cycles".to_string(), Value::from(task.max_cycles)),
                    ("message_count".to_string(), Value::from(messages.len())),
                ]),
            );
            let (prepared_messages, memory_compacted) = memory_manager.compact(&messages, false);
            messages = prepared_messages;
            let tool_schemas = self.planned_tool_schemas(&task);
            let hook_manager = self.hook_manager();
            let (request_messages, request_tool_schemas) = hook_manager.apply_before_llm(
                &task,
                cycle_index,
                messages.clone(),
                tool_schemas,
                &shared_state,
            );
            let mut request = LlmRequest::new(task.model.clone(), request_messages.clone());
            request.tools = request_tool_schemas.clone();
            let response = self.llm_client.complete(request)?;
            let response = hook_manager.apply_after_llm(
                &task,
                cycle_index,
                &request_messages,
                &request_tool_schemas,
                response,
                &shared_state,
            );
            messages = request_messages;
            messages.push(assistant_message_from_response(&response));
            let mut cycle = crate::types::CycleRecord::from_response(
                cycle_index,
                &response,
                Vec::<ToolExecutionResult>::new(),
            );
            cycle.memory_compacted = memory_compacted;
            self.emit_cycle_llm_response(&cycle);

            if response.tool_calls.is_empty() {
                cycles.push(cycle);
                match task.no_tool_policy {
                    crate::types::NoToolPolicy::Finish => {
                        self.emit_log(
                            "run_completed",
                            BTreeMap::from([
                                ("cycle".to_string(), Value::from(cycle_index)),
                                (
                                    "final_answer".to_string(),
                                    Value::String(response.content.clone()),
                                ),
                            ]),
                        );
                        return Ok(AgentResult::completed_with_shared_state(
                            messages,
                            cycles,
                            response.content.clone(),
                            shared_state,
                        ));
                    }
                    crate::types::NoToolPolicy::WaitUser => {
                        let wait_reason = if response.content.is_empty() {
                            "No tool call and runtime is waiting for user.".to_string()
                        } else {
                            response.content.clone()
                        };
                        self.emit_log(
                            "run_wait_user",
                            BTreeMap::from([
                                ("cycle".to_string(), Value::from(cycle_index)),
                                (
                                    "wait_reason".to_string(),
                                    Value::String(wait_reason.clone()),
                                ),
                            ]),
                        );
                        return Ok(AgentResult {
                            status: AgentStatus::WaitUser,
                            messages,
                            cycles,
                            final_answer: None,
                            wait_reason: Some(wait_reason),
                            error: None,
                            shared_state,
                            token_usage: crate::types::TaskTokenUsage::default(),
                        });
                    }
                    crate::types::NoToolPolicy::Continue => {
                        messages.push(crate::types::Message::user(
                            "Continue. If the task is complete, call task_finish.",
                        ));
                        continue;
                    }
                }
            }

            let sub_task_runner = self.build_sub_task_runner(
                &task,
                workspace_path.clone(),
                self.workspace_backend.clone(),
                shared_state.clone(),
                sub_task_manager.clone(),
            );
            let mut context = ToolContext {
                workspace: workspace_path.clone(),
                shared_state: shared_state.clone(),
                cycle_index,
                task_id: task.task_id.clone(),
                metadata: task.metadata.clone(),
                workspace_backend: self.workspace_backend.clone(),
                sub_task_runner,
                sub_task_manager: Some(sub_task_manager.clone()),
            };

            let mut directive_result = None;
            for call in &response.tool_calls {
                let (patched_call, short_circuit_result) =
                    hook_manager.apply_before_tool_call(&task, cycle_index, call.clone(), &context);
                let mut result = match short_circuit_result {
                    Some(result) => result,
                    None => execute_tool_result(&self.tool_registry, &patched_call, &mut context),
                };
                if result.tool_call_id.is_empty() {
                    result.tool_call_id = patched_call.id.clone();
                }
                result = hook_manager.apply_after_tool_call(
                    &task,
                    cycle_index,
                    &patched_call,
                    &context,
                    result,
                );
                if result.tool_call_id.is_empty() {
                    result.tool_call_id = patched_call.id.clone();
                }
                self.emit_tool_result(cycle_index, &patched_call, &result);

                if result.directive != ToolDirective::Continue {
                    directive_result = Some(result.clone());
                }
                messages.push(result.to_message());
                if let Some(image_url) = &result.image_url {
                    let image_path = result.image_path.as_deref().unwrap_or("image").to_string();
                    let mut image_message =
                        crate::types::Message::user(format!("[Image loaded] {image_path}"));
                    image_message.image_url = Some(image_url.clone());
                    image_message.metadata = result.metadata.clone();
                    messages.push(image_message);
                }
                cycle.tool_results.push(result);
                if directive_result.is_some() {
                    break;
                }
            }
            shared_state = context.shared_state.clone();

            cycles.push(cycle);
            if let Some(result) = directive_result {
                match result.directive {
                    ToolDirective::Finish => {
                        let final_message = extract_final_message(&result);
                        self.emit_log(
                            "run_completed",
                            BTreeMap::from([
                                ("cycle".to_string(), Value::from(cycle_index)),
                                (
                                    "final_answer".to_string(),
                                    Value::String(final_message.clone()),
                                ),
                            ]),
                        );
                        return Ok(AgentResult::completed_with_shared_state(
                            messages,
                            cycles,
                            final_message,
                            shared_state,
                        ));
                    }
                    ToolDirective::WaitUser => {
                        let wait_reason = extract_wait_reason(&result);
                        self.emit_log(
                            "run_wait_user",
                            BTreeMap::from([
                                ("cycle".to_string(), Value::from(cycle_index)),
                                (
                                    "wait_reason".to_string(),
                                    Value::String(wait_reason.clone()),
                                ),
                            ]),
                        );
                        return Ok(AgentResult {
                            status: AgentStatus::WaitUser,
                            messages,
                            cycles,
                            final_answer: None,
                            wait_reason: Some(wait_reason),
                            error: None,
                            shared_state,
                            token_usage: crate::types::TaskTokenUsage::default(),
                        });
                    }
                    ToolDirective::Continue => {}
                }
            }
        }

        self.emit_log(
            "run_max_cycles",
            BTreeMap::from([
                ("cycle".to_string(), Value::from(cycles.len())),
                (
                    "error".to_string(),
                    Value::String("maximum cycle count reached".to_string()),
                ),
            ]),
        );
        Ok(AgentResult {
            status: AgentStatus::MaxCycles,
            messages,
            cycles,
            final_answer: None,
            wait_reason: None,
            error: Some("maximum cycle count reached".to_string()),
            shared_state,
            token_usage: crate::types::TaskTokenUsage::default(),
        })
    }

    fn planned_tool_schemas(&self, task: &AgentTask) -> Vec<Value> {
        self.tool_registry.planned_openai_schemas(task)
    }

    fn hook_manager(&self) -> RuntimeHookManager {
        RuntimeHookManager::new(self.hooks.clone())
    }

    fn emit_log(&self, event: &str, payload: BTreeMap<String, Value>) {
        let Some(handler) = &self.log_handler else {
            return;
        };
        if let Ok(mut handler) = handler.lock() {
            (handler)(event, &payload);
        }
    }

    fn emit_cycle_llm_response(&self, cycle: &crate::types::CycleRecord) {
        self.emit_log(
            "cycle_llm_response",
            BTreeMap::from([
                ("cycle".to_string(), Value::from(cycle.index)),
                (
                    "assistant_message".to_string(),
                    Value::String(cycle.assistant_message.clone()),
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

    fn emit_tool_result(&self, cycle_index: u32, call: &ToolCall, result: &ToolExecutionResult) {
        self.emit_log(
            "tool_result",
            BTreeMap::from([
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
                    serde_json::to_value(result.status).unwrap_or(Value::Null),
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
                    "metadata".to_string(),
                    Value::Object(result.metadata.clone().into_iter().collect()),
                ),
            ]),
        );
    }
}

fn execute_tool_result(
    registry: &ToolRegistry,
    call: &ToolCall,
    context: &mut ToolContext,
) -> ToolExecutionResult {
    registry
        .execute(call, context)
        .unwrap_or_else(|error| ToolExecutionResult {
            tool_call_id: call.id.clone(),
            content: serde_json::json!({
                "ok": false,
                "error": error.to_string(),
            })
            .to_string(),
            status: ToolResultStatus::Error,
            directive: ToolDirective::Continue,
            error_code: Some("tool_not_found".to_string()),
            metadata: BTreeMap::new(),
            image_url: None,
            image_path: None,
        })
}

fn build_memory_manager(task: &AgentTask, workspace_path: PathBuf) -> MemoryManager {
    MemoryManager::new(MemoryManagerConfig {
        compact_threshold: task.memory_compact_threshold,
        keep_recent_messages: read_usize_metadata(
            &task.metadata,
            "memory_keep_recent_messages",
            10,
        ),
        model: task.model.clone(),
        model_context_window: read_u64_metadata(&task.metadata, "model_context_window", 200_000),
        reserved_output_tokens: read_u64_metadata(&task.metadata, "reserved_output_tokens", 16_000),
        autocompact_buffer_tokens: read_u64_metadata(
            &task.metadata,
            "autocompact_buffer_tokens",
            13_000,
        ),
        summary_event_limit: read_usize_metadata(&task.metadata, "summary_event_limit", 40),
        tool_result_compact_threshold: read_usize_metadata(
            &task.metadata,
            "tool_result_compact_threshold",
            2_000,
        ),
        tool_result_keep_last: read_usize_metadata(&task.metadata, "tool_result_keep_last", 3),
        tool_result_excerpt_head: read_usize_metadata(
            &task.metadata,
            "tool_result_excerpt_head",
            200,
        ),
        tool_result_excerpt_tail: read_usize_metadata(
            &task.metadata,
            "tool_result_excerpt_tail",
            200,
        ),
        tool_result_artifact_dir: metadata_path(
            &task.metadata,
            "tool_result_artifact_dir",
            ".memory/tool_results",
        ),
        workspace: task.use_workspace.then_some(workspace_path),
    })
}

fn read_u64_metadata(metadata: &BTreeMap<String, Value>, key: &str, default: u64) -> u64 {
    metadata
        .get(key)
        .and_then(|value| match value {
            Value::Number(number) => number.as_u64(),
            Value::String(text) => text.trim().parse::<u64>().ok(),
            _ => None,
        })
        .unwrap_or(default)
}

fn read_usize_metadata(metadata: &BTreeMap<String, Value>, key: &str, default: usize) -> usize {
    read_u64_metadata(metadata, key, default as u64) as usize
}

fn metadata_path(metadata: &BTreeMap<String, Value>, key: &str, default: &str) -> PathBuf {
    metadata
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default))
}
