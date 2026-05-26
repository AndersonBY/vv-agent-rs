mod results;
mod sub_agents;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

use crate::llm::{LlmClient, LlmError, LlmRequest};
use crate::sub_task_manager::SubTaskManager;
use crate::tools::{build_default_registry, ToolContext, ToolRegistry};
use crate::types::{
    AgentResult, AgentStatus, AgentTask, ToolDirective, ToolExecutionResult, ToolResultStatus,
};
use crate::workspace::{LocalWorkspaceBackend, WorkspaceBackend};

use results::{assistant_message_from_response, extract_final_message, extract_wait_reason};

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
}
