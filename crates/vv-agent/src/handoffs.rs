use std::sync::Arc;

use serde_json::{json, Value};

use crate::agent::Agent;
use crate::tools::{ToolContext, ToolHandler, ToolSpec, ToolSpecKind};
use crate::types::{ToolArguments, ToolDirective, ToolExecutionResult, ToolResultStatus};

#[derive(Clone)]
pub struct Handoff {
    target: Arc<Agent>,
    description: Option<String>,
    tool_name: String,
}

impl Handoff {
    pub fn target(&self) -> &Agent {
        &self.target
    }

    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    pub fn tool_name(&self) -> &str {
        &self.tool_name
    }

    pub fn as_tool_spec(&self, from_agent: &str) -> ToolSpec {
        let target = self.target.name().to_string();
        let description = self
            .description
            .clone()
            .unwrap_or_else(|| format!("Transfer the conversation to {target}."));
        let from_agent = from_agent.to_string();
        let tool_name = self.tool_name.clone();
        let target_for_handler = target.clone();
        let handler: ToolHandler = Arc::new(
            move |_context: &mut ToolContext, arguments: &ToolArguments| {
                handoff_tool_result(&from_agent, &target_for_handler, arguments)
            },
        );
        let mut spec = ToolSpec::new(tool_name.clone(), description.clone(), handler);
        spec.kind = ToolSpecKind::Handoff;
        spec.schema = json!({
            "type": "function",
            "function": {
                "name": tool_name,
                "description": description,
                "parameters": {
                    "type": "object",
                    "properties": {
                        "input": {
                            "type": "string",
                            "description": "Input or handoff summary for the target agent."
                        }
                    },
                    "required": ["input"]
                }
            }
        });
        spec
    }
}

pub fn handoff(agent: &Agent) -> HandoffBuilder {
    HandoffBuilder {
        target: Arc::new(agent.clone()),
        description: None,
        tool_name: None,
    }
}

pub struct HandoffBuilder {
    target: Arc<Agent>,
    description: Option<String>,
    tool_name: Option<String>,
}

impl HandoffBuilder {
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.tool_name = Some(name.into());
        self
    }

    pub fn build(self) -> Handoff {
        let target_name = self.target.name().to_string();
        Handoff {
            target: self.target,
            description: self.description,
            tool_name: self
                .tool_name
                .unwrap_or_else(|| format!("transfer_to_{target_name}")),
        }
    }
}

impl From<HandoffBuilder> for Handoff {
    fn from(builder: HandoffBuilder) -> Self {
        builder.build()
    }
}

fn handoff_tool_result(
    from_agent: &str,
    to_agent: &str,
    arguments: &ToolArguments,
) -> ToolExecutionResult {
    let input = arguments
        .get("input")
        .and_then(value_as_string)
        .unwrap_or_default();
    let mut metadata = std::collections::BTreeMap::new();
    metadata.insert("handoff".to_string(), Value::Bool(true));
    metadata.insert(
        "from_agent".to_string(),
        Value::String(from_agent.to_string()),
    );
    metadata.insert("to_agent".to_string(), Value::String(to_agent.to_string()));
    metadata.insert("handoff_input".to_string(), Value::String(input.clone()));
    metadata.insert(
        "final_message".to_string(),
        Value::String(format!("Handing off to {to_agent}.")),
    );
    ToolExecutionResult {
        tool_call_id: String::new(),
        content: json!({
            "ok": true,
            "handoff": true,
            "from_agent": from_agent,
            "to_agent": to_agent,
            "input": input,
        })
        .to_string(),
        status: ToolResultStatus::Success,
        directive: ToolDirective::Finish,
        error_code: None,
        metadata,
        image_url: None,
        image_path: None,
    }
}

fn value_as_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => {
            let value = value.trim();
            (!value.is_empty()).then(|| value.to_string())
        }
        other => {
            let value = other.to_string();
            (!value.is_empty()).then_some(value)
        }
    }
}
