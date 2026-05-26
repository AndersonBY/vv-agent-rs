use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

use crate::llm::{LlmClient, LlmError, LlmRequest};
use crate::sub_task_manager::SubTaskManager;
use crate::tools::{build_default_registry, SubTaskRunner, ToolContext, ToolRegistry};
use crate::types::{
    AgentResult, AgentStatus, AgentTask, LLMResponse, Message, NoToolPolicy, SubTaskOutcome,
    SubTaskRequest, ToolDirective, ToolExecutionResult, ToolResultStatus,
};
use crate::workspace::{LocalWorkspaceBackend, WorkspaceBackend};

pub type RuntimeLogHandler = Box<dyn FnMut(&str, &BTreeMap<String, Value>) + Send + Sync + 'static>;

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
}

impl<C: LlmClient> AgentRuntime<C> {
    pub fn new(llm_client: C) -> Self {
        Self {
            llm_client,
            tool_registry: build_default_registry(),
            default_workspace: None,
            log_handler: None,
            workspace_backend: Arc::new(LocalWorkspaceBackend::new(PathBuf::from("./workspace"))),
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
            messages.push(Message::system(task.system_prompt.clone()));
        }
        messages.push(Message::user(task.user_prompt.clone()));

        let mut cycles = Vec::new();
        let mut shared_state = BTreeMap::new();
        shared_state.insert("todo_list".to_string(), Value::Array(Vec::new()));
        let workspace_path = self
            .default_workspace
            .clone()
            .unwrap_or_else(|| PathBuf::from("./workspace"));
        let sub_task_manager = SubTaskManager::default();

        for cycle_index in 0..task.max_cycles {
            let tool_schemas = self.planned_tool_schemas(&task);
            let mut request = LlmRequest::new(task.model.clone(), messages.clone());
            request.tools = tool_schemas;
            let response = self.llm_client.complete(request)?;
            messages.push(assistant_message_from_response(&response));
            let mut cycle = crate::types::CycleRecord::from_response(
                cycle_index,
                &response,
                Vec::<ToolExecutionResult>::new(),
            );

            if response.tool_calls.is_empty() {
                cycles.push(cycle);
                match task.no_tool_policy {
                    crate::types::NoToolPolicy::Finish => {
                        return Ok(AgentResult::completed_with_shared_state(
                            messages,
                            cycles,
                            response.content.clone(),
                            shared_state,
                        ));
                    }
                    crate::types::NoToolPolicy::WaitUser => {
                        return Ok(AgentResult {
                            status: AgentStatus::WaitUser,
                            messages,
                            cycles,
                            final_answer: None,
                            wait_reason: Some(if response.content.is_empty() {
                                "No tool call and runtime is waiting for user.".to_string()
                            } else {
                                response.content.clone()
                            }),
                            error: None,
                            shared_state,
                            token_usage: crate::types::TaskTokenUsage::default(),
                        });
                    }
                    crate::types::NoToolPolicy::Continue => {
                        messages.push(Message::user(
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
                let mut result = self
                    .tool_registry
                    .execute(call, &mut context)
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
                    });
                if result.tool_call_id.is_empty() {
                    result.tool_call_id = call.id.clone();
                }
                if result.directive != ToolDirective::Continue {
                    directive_result = Some(result.clone());
                }
                messages.push(result.to_message());
                if let Some(image_url) = &result.image_url {
                    let image_path = result.image_path.as_deref().unwrap_or("image").to_string();
                    let mut image_message = Message::user(format!("[Image loaded] {image_path}"));
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
                        return Ok(AgentResult::completed_with_shared_state(
                            messages,
                            cycles,
                            extract_final_message(&result),
                            shared_state,
                        ));
                    }
                    ToolDirective::WaitUser => {
                        return Ok(AgentResult {
                            status: AgentStatus::WaitUser,
                            messages,
                            cycles,
                            final_answer: None,
                            wait_reason: Some(extract_wait_reason(&result)),
                            error: None,
                            shared_state,
                            token_usage: crate::types::TaskTokenUsage::default(),
                        });
                    }
                    ToolDirective::Continue => {}
                }
            }
        }

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

    fn build_sub_task_runner(
        &self,
        parent_task: &AgentTask,
        workspace_path: PathBuf,
        workspace_backend: Arc<dyn WorkspaceBackend>,
        parent_shared_state: BTreeMap<String, Value>,
        sub_task_manager: SubTaskManager,
    ) -> Option<SubTaskRunner> {
        if parent_task.sub_agents.is_empty() {
            return None;
        }
        let llm_client = self.llm_client.clone();
        let tool_registry = self.tool_registry.clone();
        let parent_task = parent_task.clone();
        let sub_task_context = SubTaskRunContext {
            tool_registry,
            workspace_backend,
            workspace_path,
            parent_task,
            parent_shared_state,
            sub_task_manager,
        };
        Some(Arc::new(move |request| {
            run_sub_task(llm_client.clone(), sub_task_context.clone(), request)
        }))
    }
}

#[derive(Clone)]
struct SubTaskRunContext {
    tool_registry: ToolRegistry,
    workspace_backend: Arc<dyn WorkspaceBackend>,
    workspace_path: PathBuf,
    parent_task: AgentTask,
    parent_shared_state: BTreeMap<String, Value>,
    sub_task_manager: SubTaskManager,
}

struct SubTaskBuildInputs<'a> {
    sub_task_id: &'a str,
    sub_session_id: &'a str,
    sub_agent_name: &'a str,
    sub_agent: &'a crate::types::SubAgentConfig,
    request: &'a SubTaskRequest,
}

fn run_sub_task<C: LlmClient + Clone + 'static>(
    llm_client: C,
    context: SubTaskRunContext,
    request: SubTaskRequest,
) -> SubTaskOutcome {
    let parent_task = &context.parent_task;
    let sub_task_id = request
        .metadata
        .get("task_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            SubTaskManager::next_task_identity(&parent_task.task_id, &request.agent_name).0
        });
    let sub_session_id = request
        .metadata
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| sub_task_id.clone());

    let Some(sub_agent) = context.parent_task.sub_agents.get(&request.agent_name) else {
        let agent_name = request.agent_name;
        let available = context
            .parent_task
            .sub_agents
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        let outcome = SubTaskOutcome {
            task_id: sub_task_id.clone(),
            agent_name: agent_name.clone(),
            status: AgentStatus::Failed,
            session_id: Some(sub_session_id),
            final_answer: None,
            wait_reason: None,
            error: Some(format!(
                "Unknown sub-agent {agent_name:?}. Available: {available}"
            )),
            cycles: 0,
            todo_list: Vec::new(),
            resolved: BTreeMap::new(),
        };
        context
            .sub_task_manager
            .record_outcome(&sub_task_id, outcome.clone());
        return outcome;
    };

    let mut sub_task = build_sub_agent_task(
        &context,
        SubTaskBuildInputs {
            sub_task_id: &sub_task_id,
            sub_session_id: &sub_session_id,
            sub_agent_name: &request.agent_name,
            sub_agent,
            request: &request,
        },
    );
    if sub_task.model.is_empty() {
        sub_task.model = context.parent_task.model.clone();
    }
    let mut sub_runtime =
        AgentRuntime::new(llm_client).with_tool_registry(context.tool_registry.clone());
    sub_runtime.default_workspace = Some(context.workspace_path.clone());
    sub_runtime.workspace_backend = context.workspace_backend.clone();

    let result = match sub_runtime.run(sub_task) {
        Ok(result) => result,
        Err(error) => {
            let outcome = SubTaskOutcome {
                task_id: sub_task_id.clone(),
                agent_name: request.agent_name,
                status: AgentStatus::Failed,
                session_id: Some(sub_session_id),
                final_answer: None,
                wait_reason: None,
                error: Some(error.to_string()),
                cycles: 0,
                todo_list: Vec::new(),
                resolved: BTreeMap::new(),
            };
            context
                .sub_task_manager
                .record_outcome(&sub_task_id, outcome.clone());
            return outcome;
        }
    };

    let todo_list = result.todo_list();
    let cycles = result.cycles.len() as u32;
    let outcome = SubTaskOutcome {
        task_id: sub_task_id.clone(),
        agent_name: request.agent_name,
        status: result.status,
        session_id: Some(sub_session_id),
        final_answer: result.final_answer,
        wait_reason: result.wait_reason,
        error: result.error,
        cycles,
        todo_list,
        resolved: BTreeMap::from([
            ("model".to_string(), sub_agent.model.clone()),
            (
                "backend".to_string(),
                sub_agent.backend.clone().unwrap_or_default(),
            ),
        ]),
    };
    context
        .sub_task_manager
        .record_outcome(&sub_task_id, outcome.clone());
    outcome
}

fn build_sub_agent_task(context: &SubTaskRunContext, inputs: SubTaskBuildInputs<'_>) -> AgentTask {
    let parent_task = &context.parent_task;
    let sub_agent = inputs.sub_agent;
    let request = inputs.request;
    let system_prompt = sub_agent.system_prompt.clone().unwrap_or_else(|| {
        crate::prompt::build_system_prompt_bundle(&sub_agent.description).prompt
    });
    let mut user_prompt = request.task_description.clone();
    if !request.output_requirements.is_empty() {
        user_prompt.push_str("\n\n<Output Requirements>\n");
        user_prompt.push_str(&request.output_requirements);
        user_prompt.push_str("\n</Output Requirements>");
    }
    if request.include_main_summary {
        let parent_summary = build_parent_summary(parent_task, &context.parent_shared_state);
        if !parent_summary.is_empty() {
            user_prompt.push_str("\n\n<Main Task Summary>\n");
            user_prompt.push_str(&parent_summary);
            user_prompt.push_str("\n</Main Task Summary>");
        }
    }

    let mut sub_task = AgentTask::new(
        inputs.sub_task_id,
        sub_agent.model.clone(),
        system_prompt,
        user_prompt,
    );
    sub_task.max_cycles = sub_agent.max_cycles.max(1);
    sub_task.memory_compact_threshold = parent_task.memory_compact_threshold;
    sub_task.memory_threshold_percentage = parent_task.memory_threshold_percentage;
    sub_task.no_tool_policy = NoToolPolicy::Continue;
    sub_task.allow_interruption = false;
    sub_task.use_workspace = parent_task.use_workspace;
    sub_task.has_sub_agents = false;
    sub_task.sub_agents = BTreeMap::new();
    sub_task.agent_type = parent_task.agent_type.clone();
    sub_task.native_multimodal = parent_task.native_multimodal;
    sub_task.extra_tool_names = parent_task.extra_tool_names.clone();
    sub_task.exclude_tools = merged_sub_task_exclusions(parent_task, sub_agent);
    sub_task.metadata = build_sub_task_metadata(
        parent_task,
        inputs.sub_task_id,
        inputs.sub_session_id,
        inputs.sub_agent_name,
        request,
        &context.workspace_path,
    );
    sub_task
}

fn merged_sub_task_exclusions(
    parent_task: &AgentTask,
    sub_agent: &crate::types::SubAgentConfig,
) -> Vec<String> {
    let mut excluded = parent_task.exclude_tools.clone();
    excluded.extend(sub_agent.exclude_tools.clone());
    excluded.push(crate::constants::CREATE_SUB_TASK_TOOL_NAME.to_string());
    excluded.push(crate::constants::SUB_TASK_STATUS_TOOL_NAME.to_string());
    excluded.sort();
    excluded.dedup();
    excluded
}

fn build_sub_task_metadata(
    parent_task: &AgentTask,
    sub_task_id: &str,
    sub_session_id: &str,
    sub_agent_name: &str,
    request: &SubTaskRequest,
    workspace_path: &std::path::Path,
) -> BTreeMap<String, Value> {
    let mut metadata = BTreeMap::from([
        ("is_sub_task".to_string(), Value::Bool(true)),
        (
            "parent_task_id".to_string(),
            Value::String(parent_task.task_id.clone()),
        ),
        (
            "sub_agent_name".to_string(),
            Value::String(sub_agent_name.to_string()),
        ),
        ("session_memory_enabled".to_string(), Value::Bool(false)),
        (
            "workspace".to_string(),
            Value::String(workspace_path.display().to_string()),
        ),
    ]);
    for key in [
        "bash_shell",
        "windows_shell_priority",
        "bash_env",
        "allow_outside_workspace_paths",
        "allow_outside_workspace",
        "workspace_allow_outside_main",
        "workspace_allow_outside",
        "language",
        "available_skills",
        "active_skills",
    ] {
        if let Some(value) = parent_task.metadata.get(key) {
            metadata.insert(key.to_string(), value.clone());
        }
    }
    if let Some(sub_agent) = parent_task.sub_agents.get(sub_agent_name) {
        metadata.extend(sub_agent.metadata.clone());
    }
    metadata.extend(request.metadata.clone());
    metadata.insert(
        "task_id".to_string(),
        Value::String(sub_task_id.to_string()),
    );
    metadata.insert(
        "session_id".to_string(),
        Value::String(sub_session_id.to_string()),
    );
    metadata.insert(
        "browser_scope_key".to_string(),
        Value::String(sub_session_id.to_string()),
    );
    metadata
}

fn build_parent_summary(
    parent_task: &AgentTask,
    parent_shared_state: &BTreeMap<String, Value>,
) -> String {
    let mut lines = vec![format!("Parent task goal: {}", parent_task.user_prompt)];
    if let Some(todo_list) = parent_shared_state
        .get("todo_list")
        .and_then(Value::as_array)
    {
        if !todo_list.is_empty() {
            lines.push("Parent TODO status:".to_string());
            for item in todo_list {
                let title = item
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("Untitled");
                let status = item
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("pending");
                lines.push(format!("- [{status}] {title}"));
            }
        }
    }
    lines.join("\n")
}

fn assistant_message_from_response(response: &LLMResponse) -> Message {
    let mut message = Message::assistant(response.content.clone());
    message.tool_calls = response.tool_calls.clone();
    message
}

fn extract_final_message(result: &ToolExecutionResult) -> String {
    result
        .metadata
        .get("final_message")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            serde_json::from_str::<Value>(&result.content)
                .ok()
                .and_then(|value| {
                    value
                        .get("message")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
        })
        .unwrap_or_else(|| result.content.clone())
}

fn extract_wait_reason(result: &ToolExecutionResult) -> String {
    result
        .metadata
        .get("question")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| result.content.clone())
}
