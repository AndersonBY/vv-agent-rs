pub mod backends;
pub mod hooks;
mod results;
pub mod state;
pub mod stores;
mod sub_agents;
mod tool_planner;

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::llm::{LlmClient, LlmError, LlmRequest};
use crate::memory::{
    MemoryManager, MemoryManagerConfig, SessionMemory, SessionMemoryConfig,
    SessionMemoryExtractionCallback,
};
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
pub use tool_planner::patch_dynamic_tool_schema_hints;

pub type RuntimeLogCallback = dyn FnMut(&str, &BTreeMap<String, Value>) + Send + Sync + 'static;
pub type RuntimeLogHandler = Arc<Mutex<Box<RuntimeLogCallback>>>;
pub type RuntimeEventHandler = Arc<dyn Fn(&str, &BTreeMap<String, Value>) + Send + Sync + 'static>;
const MAX_PROMPT_TOO_LONG_RETRIES: u32 = 3;

#[derive(Clone, Default)]
pub struct RuntimeRunControls {
    pub log_handler: Option<RuntimeEventHandler>,
    pub steering_queue: Option<Arc<Mutex<VecDeque<String>>>>,
    pub cancellation_token: Option<CancellationToken>,
}

#[derive(Clone, Default)]
pub struct CancellationToken {
    inner: Arc<CancellationState>,
}

#[derive(Default)]
struct CancellationState {
    cancelled: AtomicBool,
    callbacks: Mutex<Vec<Arc<dyn Fn() + Send + Sync + 'static>>>,
}

impl std::fmt::Debug for CancellationToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CancellationToken")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

impl CancellationToken {
    pub fn cancel(&self) {
        if self.inner.cancelled.swap(true, Ordering::SeqCst) {
            return;
        }
        let callbacks = std::mem::take(
            &mut *self
                .inner
                .callbacks
                .lock()
                .expect("cancellation callbacks lock"),
        );
        for callback in callbacks {
            callback();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }

    pub fn check(&self) -> Result<(), String> {
        if self.is_cancelled() {
            Err("Operation was cancelled".to_string())
        } else {
            Ok(())
        }
    }

    pub fn on_cancel(&self, callback: impl Fn() + Send + Sync + 'static) {
        let callback: Arc<dyn Fn() + Send + Sync + 'static> = Arc::new(callback);
        let call_immediately = {
            let mut callbacks = self
                .inner
                .callbacks
                .lock()
                .expect("cancellation callbacks lock");
            if self.is_cancelled() {
                true
            } else {
                callbacks.push(callback.clone());
                false
            }
        };
        if call_immediately {
            callback();
        }
    }

    pub fn child(&self) -> Self {
        let child = Self::default();
        let child_to_cancel = child.clone();
        self.on_cancel(move || child_to_cancel.cancel());
        child
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
        self.run_with_controls(task, RuntimeRunControls::default())
    }

    pub fn run_with_controls(
        &self,
        task: AgentTask,
        controls: RuntimeRunControls,
    ) -> Result<AgentResult, LlmError> {
        let mut messages = build_initial_messages(&task);

        let mut cycles = Vec::new();
        let mut shared_state = task.initial_shared_state.clone();
        shared_state
            .entry("todo_list".to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        let workspace_path = self
            .default_workspace
            .clone()
            .unwrap_or_else(|| PathBuf::from("./workspace"));
        let sub_task_manager = SubTaskManager::default();
        self.emit_log(
            &controls,
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

        let mut memory_manager =
            build_memory_manager(&task, workspace_path.clone(), Some(self.llm_client.clone()));

        if controls_cancelled(&controls) {
            self.emit_log(
                &controls,
                "run_cancelled",
                BTreeMap::from([(
                    "error".to_string(),
                    Value::String("Operation was cancelled".to_string()),
                )]),
            );
            return Ok(cancelled_agent_result(messages, cycles, shared_state));
        }

        for cycle_index in 0..task.max_cycles {
            if controls_cancelled(&controls) {
                self.emit_log(
                    &controls,
                    "run_cancelled",
                    BTreeMap::from([
                        ("cycle".to_string(), Value::from(cycle_index)),
                        (
                            "error".to_string(),
                            Value::String("Operation was cancelled".to_string()),
                        ),
                    ]),
                );
                return Ok(cancelled_agent_result(messages, cycles, shared_state));
            }
            let cycle_steering_prompts = drain_steering_queue(&controls);
            if !cycle_steering_prompts.is_empty() {
                for prompt in &cycle_steering_prompts {
                    messages.push(crate::types::Message::user(prompt.clone()));
                    self.emit_log(
                        &controls,
                        "session_steer_dequeued",
                        BTreeMap::from([
                            ("cycle".to_string(), Value::from(cycle_index)),
                            ("prompt".to_string(), Value::String(prompt.clone())),
                        ]),
                    );
                }
                self.emit_log(
                    &controls,
                    "cycle_injected_messages",
                    BTreeMap::from([
                        ("cycle".to_string(), Value::from(cycle_index)),
                        (
                            "reason".to_string(),
                            Value::String("session_steering".to_string()),
                        ),
                        (
                            "message_count".to_string(),
                            Value::from(cycle_steering_prompts.len() as u64),
                        ),
                    ]),
                );
            }
            self.emit_log(
                &controls,
                "cycle_started",
                BTreeMap::from([
                    ("cycle".to_string(), Value::from(cycle_index)),
                    ("max_cycles".to_string(), Value::from(task.max_cycles)),
                    ("message_count".to_string(), Value::from(messages.len())),
                ]),
            );
            let (prepared_messages, memory_compacted) =
                memory_manager.compact_for_cycle(&messages, cycle_index, false);
            messages = prepared_messages;
            let tool_schemas = self.planned_tool_schemas(&task);
            let hook_manager = self.hook_manager();
            let llm_messages = memory_manager.apply_session_memory_context(&messages);
            let (request_messages, request_tool_schemas) = hook_manager.apply_before_llm(
                &task,
                cycle_index,
                llm_messages,
                tool_schemas,
                &shared_state,
            );
            let mut request_messages = request_messages;
            let mut request_tool_schemas = request_tool_schemas;
            let mut memory_compacted = memory_compacted;
            let mut prompt_too_long_retries = 0;
            let response = loop {
                let mut request = LlmRequest::new(task.model.clone(), request_messages.clone());
                request.tools = request_tool_schemas.clone();
                match self.llm_client.complete(request) {
                    Ok(response) => break response,
                    Err(error) if is_prompt_too_long_error(&error) => {
                        prompt_too_long_retries += 1;
                        if prompt_too_long_retries > MAX_PROMPT_TOO_LONG_RETRIES {
                            return Err(error);
                        }
                        memory_compacted = true;
                        let retry_messages = if prompt_too_long_retries == 1 {
                            let (compacted, _) = memory_manager.compact_for_cycle(
                                &request_messages,
                                cycle_index,
                                true,
                            );
                            compacted
                        } else {
                            memory_manager.emergency_compact(
                                &request_messages,
                                (0.2 * prompt_too_long_retries as f64).min(0.95),
                            )
                        };
                        let retry_tool_schemas = self.planned_tool_schemas(&task);
                        let llm_messages =
                            memory_manager.apply_session_memory_context(&retry_messages);
                        (request_messages, request_tool_schemas) = hook_manager.apply_before_llm(
                            &task,
                            cycle_index,
                            llm_messages,
                            retry_tool_schemas,
                            &shared_state,
                        );
                    }
                    Err(error) => return Err(error),
                }
            };
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
            self.emit_cycle_llm_response(&controls, &cycle);

            if response.tool_calls.is_empty() {
                cycles.push(cycle);
                match task.no_tool_policy {
                    crate::types::NoToolPolicy::Finish => {
                        self.emit_log(
                            &controls,
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
                            &controls,
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
            for (call_index, call) in response.tool_calls.iter().enumerate() {
                if controls_cancelled(&controls) {
                    shared_state = context.shared_state.clone();
                    cycles.push(cycle);
                    self.emit_log(
                        &controls,
                        "run_cancelled",
                        BTreeMap::from([
                            ("cycle".to_string(), Value::from(cycle_index)),
                            (
                                "error".to_string(),
                                Value::String("Operation was cancelled".to_string()),
                            ),
                        ]),
                    );
                    return Ok(cancelled_agent_result(messages, cycles, shared_state));
                }
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
                self.emit_tool_result(&controls, cycle_index, &patched_call, &result);

                let steering_prompts = drain_steering_queue(&controls);
                if steering_prompts.is_empty() && result.directive != ToolDirective::Continue {
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
                if !steering_prompts.is_empty() {
                    for prompt in &steering_prompts {
                        self.emit_log(
                            &controls,
                            "session_steer_interrupt",
                            BTreeMap::from([
                                ("cycle".to_string(), Value::from(cycle_index)),
                                (
                                    "after_tool_call_id".to_string(),
                                    Value::String(patched_call.id.clone()),
                                ),
                                (
                                    "after_tool_name".to_string(),
                                    Value::String(patched_call.name.clone()),
                                ),
                                ("prompt".to_string(), Value::String(prompt.clone())),
                            ]),
                        );
                    }
                    for skipped_call in response.tool_calls.iter().skip(call_index + 1) {
                        let skipped = skipped_tool_result(
                            skipped_call,
                            "skipped_due_to_steering",
                            "Tool skipped because session steering was queued after a previous tool call.",
                        );
                        self.emit_tool_result(&controls, cycle_index, skipped_call, &skipped);
                        messages.push(skipped.to_message());
                        cycle.tool_results.push(skipped);
                    }
                    for prompt in &steering_prompts {
                        messages.push(crate::types::Message::user(prompt.clone()));
                    }
                    self.emit_log(
                        &controls,
                        "run_steered",
                        BTreeMap::from([
                            ("cycle".to_string(), Value::from(cycle_index)),
                            (
                                "after_tool_call_id".to_string(),
                                Value::String(patched_call.id.clone()),
                            ),
                            (
                                "after_tool_name".to_string(),
                                Value::String(patched_call.name.clone()),
                            ),
                            (
                                "prompt_count".to_string(),
                                Value::from(steering_prompts.len() as u64),
                            ),
                        ]),
                    );
                    break;
                }
                if directive_result.is_some() {
                    let (error_code, message) = match directive_result
                        .as_ref()
                        .map(|result| result.directive)
                        .unwrap_or(ToolDirective::Continue)
                    {
                        ToolDirective::WaitUser => (
                            "skipped_due_to_wait_user",
                            "Tool skipped because a previous tool requested user input.",
                        ),
                        ToolDirective::Finish => (
                            "skipped_due_to_finish",
                            "Tool skipped because a previous tool finished the task.",
                        ),
                        ToolDirective::Continue => ("skipped_due_to_directive", "Tool skipped."),
                    };
                    for skipped_call in response.tool_calls.iter().skip(call_index + 1) {
                        let skipped = skipped_tool_result(skipped_call, error_code, message);
                        self.emit_tool_result(&controls, cycle_index, skipped_call, &skipped);
                        messages.push(skipped.to_message());
                        cycle.tool_results.push(skipped);
                    }
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
                            &controls,
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
                            &controls,
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
            &controls,
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

    fn emit_log(
        &self,
        controls: &RuntimeRunControls,
        event: &str,
        payload: BTreeMap<String, Value>,
    ) {
        if let Some(handler) = &self.log_handler {
            if let Ok(mut handler) = handler.lock() {
                (handler)(event, &payload);
            }
        }
        if let Some(handler) = &controls.log_handler {
            handler(event, &payload);
        }
    }

    fn emit_cycle_llm_response(
        &self,
        controls: &RuntimeRunControls,
        cycle: &crate::types::CycleRecord,
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

    fn emit_tool_result(
        &self,
        controls: &RuntimeRunControls,
        cycle_index: u32,
        call: &ToolCall,
        result: &ToolExecutionResult,
    ) {
        self.emit_log(
            controls,
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

fn drain_steering_queue(controls: &RuntimeRunControls) -> Vec<String> {
    let Some(queue) = &controls.steering_queue else {
        return Vec::new();
    };
    let Ok(mut queue) = queue.lock() else {
        return Vec::new();
    };
    queue.drain(..).collect()
}

fn controls_cancelled(controls: &RuntimeRunControls) -> bool {
    controls
        .cancellation_token
        .as_ref()
        .is_some_and(CancellationToken::is_cancelled)
}

fn cancelled_agent_result(
    messages: Vec<crate::types::Message>,
    cycles: Vec<crate::types::CycleRecord>,
    shared_state: BTreeMap<String, Value>,
) -> AgentResult {
    AgentResult {
        status: AgentStatus::Failed,
        messages,
        cycles,
        final_answer: None,
        wait_reason: None,
        error: Some("Operation was cancelled".to_string()),
        shared_state,
        token_usage: crate::types::TaskTokenUsage::default(),
    }
}

fn is_prompt_too_long_error(error: &LlmError) -> bool {
    let text = error.to_string().to_ascii_lowercase();
    [
        "prompt is too long",
        "prompt_too_long",
        "context_length_exceeded",
        "maximum context length",
        "request too large",
        "too many tokens",
    ]
    .iter()
    .any(|pattern| text.contains(pattern))
}

fn build_initial_messages(task: &AgentTask) -> Vec<crate::types::Message> {
    if !task.initial_messages.is_empty() {
        let mut messages = task.initial_messages.clone();
        let starts_with_system = messages
            .first()
            .is_some_and(|message| message.role == crate::types::MessageRole::System);
        if !starts_with_system && !task.system_prompt.is_empty() {
            messages.insert(0, system_message_from_task(task));
        } else if starts_with_system && !task.metadata.is_empty() {
            if let Some(system_message) = messages.first_mut() {
                let mut metadata = task.metadata.clone();
                metadata.extend(system_message.metadata.clone());
                system_message.metadata = metadata;
            }
        }
        if !task.user_prompt.is_empty() {
            messages.push(crate::types::Message::user(task.user_prompt.clone()));
        }
        return messages;
    }

    let mut messages = Vec::new();
    if !task.system_prompt.is_empty() {
        messages.push(system_message_from_task(task));
    }
    messages.push(crate::types::Message::user(task.user_prompt.clone()));
    messages
}

fn system_message_from_task(task: &AgentTask) -> crate::types::Message {
    let mut message = crate::types::Message::system(task.system_prompt.clone());
    message.metadata = task.metadata.clone();
    message
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

fn skipped_tool_result(call: &ToolCall, error_code: &str, message: &str) -> ToolExecutionResult {
    ToolExecutionResult {
        tool_call_id: call.id.clone(),
        content: serde_json::json!({
            "ok": false,
            "error": message,
            "skipped_tool": call.name,
        })
        .to_string(),
        status: ToolResultStatus::Error,
        directive: ToolDirective::Continue,
        error_code: Some(error_code.to_string()),
        metadata: BTreeMap::new(),
        image_url: None,
        image_path: None,
    }
}

fn build_memory_manager<C>(
    task: &AgentTask,
    workspace_path: PathBuf,
    memory_summary_client: Option<C>,
) -> MemoryManager
where
    C: LlmClient + Clone + 'static,
{
    let workspace = task.use_workspace.then_some(workspace_path.clone());
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
        tool_calls_keep_last: read_usize_metadata(&task.metadata, "tool_calls_keep_last", 3),
        assistant_no_tool_keep_last: read_usize_metadata(
            &task.metadata,
            "assistant_no_tool_keep_last",
            1,
        ),
        tool_result_artifact_dir: metadata_path(
            &task.metadata,
            "tool_result_artifact_dir",
            ".memory/tool_results",
        ),
        microcompact_trigger_ratio: task
            .metadata
            .get("microcompact_trigger_ratio")
            .and_then(Value::as_f64)
            .unwrap_or(0.75),
        microcompact_keep_recent_cycles: read_usize_metadata(
            &task.metadata,
            "microcompact_keep_recent_cycles",
            3,
        ),
        microcompact_min_result_length: read_usize_metadata(
            &task.metadata,
            "microcompact_min_result_length",
            500,
        ),
        workspace: workspace.clone(),
        session_memory: build_session_memory(task, workspace, memory_summary_client),
    })
}

fn build_session_memory<C>(
    task: &AgentTask,
    workspace: Option<PathBuf>,
    memory_summary_client: Option<C>,
) -> Option<SessionMemory>
where
    C: LlmClient + Clone + 'static,
{
    if !read_bool_metadata(&task.metadata, "session_memory_enabled", false)
        && !read_bool_metadata(&task.metadata, "enable_session_memory", false)
        && !task.metadata.contains_key("session_memory_seed")
    {
        return None;
    }
    let extraction_model = task
        .metadata
        .get("session_memory_extraction_model")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            task.metadata
                .get("memory_summary_model")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| task.model.clone());
    let extraction_callback =
        memory_summary_client.map(|client| build_session_memory_extraction_callback(client));
    let mut session_memory = SessionMemory::with_workspace(
        SessionMemoryConfig {
            min_tokens_before_extraction: read_u64_metadata(
                &task.metadata,
                "session_memory_min_tokens",
                10_000,
            ),
            max_tokens: read_u64_metadata(&task.metadata, "session_memory_max_tokens", 40_000),
            min_text_messages: read_usize_metadata(
                &task.metadata,
                "session_memory_min_text_messages",
                5,
            ),
            growth_ratio: task
                .metadata
                .get("session_memory_growth_ratio")
                .and_then(Value::as_f64)
                .unwrap_or(0.5)
                .max(0.0),
            storage_dir: metadata_path(
                &task.metadata,
                "session_memory_storage_dir",
                ".memory/session",
            ),
            extraction_callback,
            extraction_backend: task
                .metadata
                .get("session_memory_extraction_backend")
                .and_then(Value::as_str)
                .map(str::to_string),
            extraction_model: Some(extraction_model),
            token_model: task.model.clone(),
        },
        workspace,
        Some(task.task_id.clone()),
    );
    session_memory.load();
    seed_session_memory(
        &mut session_memory,
        task.metadata.get("session_memory_seed"),
    );
    Some(session_memory)
}

fn build_session_memory_extraction_callback<C>(client: C) -> SessionMemoryExtractionCallback
where
    C: LlmClient + Clone + 'static,
{
    Arc::new(move |prompt, _backend, model| {
        let request = LlmRequest::new(
            model.unwrap_or_default(),
            vec![crate::types::Message::user(prompt.to_string())],
        );
        client
            .complete(request)
            .ok()
            .map(|response| response.content.trim().to_string())
            .filter(|content| !content.is_empty())
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

fn read_bool_metadata(metadata: &BTreeMap<String, Value>, key: &str, default: bool) -> bool {
    metadata
        .get(key)
        .and_then(Value::as_bool)
        .unwrap_or(default)
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

fn seed_session_memory(session_memory: &mut SessionMemory, value: Option<&Value>) {
    let Some(entries) = value.and_then(Value::as_array) else {
        return;
    };
    let parsed = entries
        .iter()
        .filter_map(|entry| {
            let object = entry.as_object()?;
            let content = object.get("content")?.as_str()?.trim();
            if content.is_empty() {
                return None;
            }
            let category = object
                .get("category")
                .and_then(Value::as_str)
                .unwrap_or("key_fact");
            let source_cycle = object
                .get("source_cycle")
                .and_then(Value::as_i64)
                .unwrap_or(0) as i32;
            let importance = object
                .get("importance")
                .and_then(Value::as_u64)
                .unwrap_or(5)
                .clamp(1, 10) as u8;
            Some(crate::memory::SessionMemoryEntry::new(
                category,
                content,
                source_cycle,
                importance,
            ))
        })
        .collect::<Vec<_>>();
    session_memory.merge_entries(parsed);
}
