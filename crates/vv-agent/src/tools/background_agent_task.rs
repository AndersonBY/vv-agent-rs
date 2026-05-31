use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::agent::Agent;
use crate::result::RunResult;
use crate::runner::{NormalizedInput, Runner};
use crate::tools::{Tool, ToolContext, ToolOutput, ToolSpec};
use crate::types::{AgentStatus, ToolArguments};

static NEXT_BACKGROUND_AGENT_TASK_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub struct BackgroundAgentTask {
    agent: Agent,
    name: String,
    description: String,
    parameters_schema: Value,
}

impl BackgroundAgentTask {
    pub fn start(
        &self,
        runner: &Runner,
        _context: &mut ToolContext,
        raw_arguments: Value,
    ) -> Result<BackgroundAgentTaskHandle, String> {
        let input = self.input_from_arguments(raw_arguments)?;
        let task_id = format!(
            "bg_agent_{:012x}",
            NEXT_BACKGROUND_AGENT_TASK_ID.fetch_add(1, Ordering::Relaxed)
        );
        let state = Arc::new(Mutex::new(BackgroundAgentTaskState {
            status: AgentStatus::Running,
            result: None,
            error: None,
        }));
        let state_for_worker = state.clone();
        let runner = runner.clone();
        let agent = self.agent.clone();
        let task_id_for_error = task_id.clone();
        let _ = std::thread::Builder::new()
            .name(format!("vv-agent-background-{task_id}"))
            .spawn(move || {
                let result = runner.run_blocking(
                    &agent,
                    NormalizedInput::from(input),
                    crate::run_config::RunConfig::default(),
                    None,
                );
                if let Ok(mut state) = state_for_worker.lock() {
                    match result {
                        Ok(result) => {
                            state.status = result.status();
                            state.result = Some(result);
                        }
                        Err(error) => {
                            state.status = AgentStatus::Failed;
                            state.error = Some(error);
                        }
                    }
                }
            })
            .map_err(|error| {
                if let Ok(mut state) = state.lock() {
                    state.status = AgentStatus::Failed;
                    state.error = Some(error.to_string());
                }
                format!("failed to spawn background agent task {task_id_for_error}: {error}")
            })?;
        Ok(BackgroundAgentTaskHandle {
            task_id,
            agent_name: self.agent.name().to_string(),
            state,
        })
    }

    fn input_from_arguments(&self, raw_arguments: Value) -> Result<String, String> {
        let object = raw_arguments
            .as_object()
            .ok_or_else(|| "background task arguments must be an object".to_string())?;
        object
            .get("task_description")
            .or_else(|| object.get("task"))
            .or_else(|| object.get("input"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .ok_or_else(|| "background task requires task_description".to_string())
    }
}

impl Tool for BackgroundAgentTask {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> &Value {
        &self.parameters_schema
    }

    fn as_tool_spec(&self) -> ToolSpec {
        let name = self.name.clone();
        let description = self.description.clone();
        let parameters_schema = self.parameters_schema.clone();
        let task = self.clone();
        let mut spec = ToolSpec::new(
            name.clone(),
            description.clone(),
            Arc::new(
                move |_context: &mut ToolContext, arguments: &ToolArguments| {
                    let raw_arguments = Value::Object(arguments.clone().into_iter().collect());
                    match task.input_from_arguments(raw_arguments) {
                        Ok(task_description) => ToolOutput::json(json!({
                            "agent_name": task.agent.name(),
                            "status": "background_task_requested",
                            "task_description": task_description,
                        }))
                        .to_result(""),
                        Err(error) => ToolOutput::error(error)
                            .with_code("invalid_background_task_arguments")
                            .to_result(""),
                    }
                },
            ),
        );
        spec.schema = json!({
            "type": "function",
            "function": {
                "name": name,
                "description": description,
                "parameters": parameters_schema,
            }
        });
        spec
    }
}

pub struct BackgroundAgentTaskBuilder {
    agent: Agent,
    name: Option<String>,
    description: Option<String>,
}

impl BackgroundAgentTaskBuilder {
    pub fn new(agent: Agent) -> Self {
        Self {
            agent,
            name: None,
            description: None,
        }
    }

    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn build(self) -> Result<BackgroundAgentTask, String> {
        let name = self
            .name
            .unwrap_or_else(|| format!("{}_background_task", self.agent.name()));
        if name.trim().is_empty() {
            return Err("background task tool name cannot be empty".to_string());
        }
        let description = self.description.unwrap_or_else(|| {
            format!(
                "Start the {} agent as a background task.",
                self.agent.name()
            )
        });
        Ok(BackgroundAgentTask {
            agent: self.agent,
            name,
            description,
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "task_description": {
                        "type": "string",
                        "description": "Task for the background agent."
                    }
                },
                "required": ["task_description"]
            }),
        })
    }
}

#[derive(Clone)]
pub struct BackgroundAgentTaskHandle {
    task_id: String,
    agent_name: String,
    state: Arc<Mutex<BackgroundAgentTaskState>>,
}

impl BackgroundAgentTaskHandle {
    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    pub fn agent_name(&self) -> &str {
        &self.agent_name
    }

    pub fn status(&self) -> AgentStatus {
        self.state
            .lock()
            .map(|state| state.status)
            .unwrap_or(AgentStatus::Failed)
    }

    pub fn poll(&self) -> Result<BackgroundAgentTaskSnapshot, String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "background task lock poisoned".to_string())?;
        Ok(BackgroundAgentTaskSnapshot {
            task_id: self.task_id.clone(),
            agent_name: self.agent_name.clone(),
            status: state.status,
            final_output: state
                .result
                .as_ref()
                .and_then(|result| result.final_output().map(str::to_string)),
            error: state.error.clone(),
        })
    }

    pub async fn wait(&self) -> Result<BackgroundAgentTaskSnapshot, String> {
        loop {
            let snapshot = self.poll()?;
            if !matches!(snapshot.status, AgentStatus::Running | AgentStatus::Pending) {
                return Ok(snapshot);
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
}

struct BackgroundAgentTaskState {
    status: AgentStatus,
    result: Option<RunResult>,
    error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundAgentTaskSnapshot {
    task_id: String,
    agent_name: String,
    status: AgentStatus,
    final_output: Option<String>,
    error: Option<String>,
}

impl BackgroundAgentTaskSnapshot {
    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    pub fn agent_name(&self) -> &str {
        &self.agent_name
    }

    pub fn status(&self) -> AgentStatus {
        self.status
    }

    pub fn final_output(&self) -> Option<&str> {
        self.final_output.as_deref()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}
