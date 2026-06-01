use std::sync::Arc;

use serde_json::{json, Value};

use crate::agent::Agent;
use crate::run_config::RunConfig;
use crate::runner::Runner;
use crate::tools::{Tool, ToolContext, ToolOutput, ToolSpec, ToolSpecKind};
use crate::types::{SubTaskRequest, ToolArguments};

#[derive(Clone)]
pub struct AgentTool {
    agent: Agent,
    name: String,
    description: String,
    parameters_schema: Value,
}

impl AgentTool {
    pub fn request_from_arguments(&self, raw_arguments: Value) -> Result<SubTaskRequest, String> {
        let object = raw_arguments
            .as_object()
            .ok_or_else(|| "agent tool arguments must be an object".to_string())?;
        let task_description = object
            .get("task_description")
            .or_else(|| object.get("task"))
            .or_else(|| object.get("input"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "agent tool requires task_description".to_string())?;
        let mut request = SubTaskRequest::new(self.agent.name(), task_description);
        request.output_requirements = object
            .get("output_requirements")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        request.include_main_summary = object
            .get("include_main_summary")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Ok(request)
    }
}

impl Tool for AgentTool {
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
        let agent_tool = self.clone();
        let mut spec = ToolSpec::new(
            self.name.clone(),
            self.description.clone(),
            Arc::new(
                move |context: &mut ToolContext, arguments: &ToolArguments| {
                    let raw_arguments = Value::Object(arguments.clone().into_iter().collect());
                    let request = match agent_tool.request_from_arguments(raw_arguments) {
                        Ok(request) => request,
                        Err(error) => return ToolOutput::error(error).to_result(""),
                    };
                    if let Some(provider) = context.model_provider.clone() {
                        let runner = Runner::builder()
                            .model_provider_arc(provider)
                            .workspace(context.workspace.clone())
                            .build();
                        let runner = match runner {
                            Ok(runner) => runner,
                            Err(error) => return ToolOutput::error(error).to_result(""),
                        };
                        let result = runner.run_blocking(
                            &agent_tool.agent,
                            crate::runner::NormalizedInput::from(request.task_description.clone()),
                            RunConfig::builder()
                                .workspace_backend(context.workspace_backend.clone())
                                .build(),
                            None,
                        );
                        return match result {
                            Ok(result) => {
                                let output = result.final_output().unwrap_or_default();
                                ToolOutput::text(output).to_result("")
                            }
                            Err(error) => ToolOutput::error(error).to_result(""),
                        };
                    }
                    let Some(runner) = context.sub_task_runner.clone() else {
                        return ToolOutput::error("sub-agent runtime is not available")
                            .with_code("sub_agents_not_enabled")
                            .to_result("");
                    };
                    let outcome = runner(request);
                    ToolOutput::json(outcome.to_value()).to_result("")
                },
            ),
        );
        spec.kind = ToolSpecKind::Agent;
        spec.schema = json!({
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": self.parameters_schema,
            }
        });
        spec
    }
}

pub struct AgentToolBuilder {
    agent: Agent,
    name: Option<String>,
    description: Option<String>,
}

impl AgentToolBuilder {
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

    pub fn build(self) -> Result<AgentTool, String> {
        let name = self.name.unwrap_or_else(|| self.agent.name().to_string());
        if name.trim().is_empty() {
            return Err("agent tool name cannot be empty".to_string());
        }
        let description = self
            .description
            .unwrap_or_else(|| format!("Run the {} agent as a delegated task.", self.agent.name()));
        Ok(AgentTool {
            agent: self.agent,
            name,
            description,
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "task_description": {
                        "type": "string",
                        "description": "Task for the delegated agent."
                    },
                    "output_requirements": {
                        "type": "string",
                        "description": "Optional output requirements for the delegated agent."
                    },
                    "include_main_summary": {
                        "type": "boolean",
                        "description": "Whether to include parent task summary."
                    }
                },
                "required": ["task_description"]
            }),
        })
    }
}
