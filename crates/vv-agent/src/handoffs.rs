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
    metadata: std::collections::BTreeMap<String, Value>,
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

    pub fn metadata(&self) -> &std::collections::BTreeMap<String, Value> {
        &self.metadata
    }

    pub fn as_tool_spec(&self, from_agent: &str) -> ToolSpec {
        let target = self.target.name().to_string();
        let description = self
            .description
            .clone()
            .unwrap_or_else(|| format!("Transfer the conversation to {target}."));
        let from_agent = from_agent.to_string();
        let tool_name = self.tool_name.clone();
        let tool_name_for_handler = tool_name.clone();
        let handoff_metadata = self.metadata.clone();
        let target_for_handler = target.clone();
        let handler: ToolHandler = Arc::new(
            move |_context: &mut ToolContext, arguments: &ToolArguments| {
                handoff_tool_result(
                    &from_agent,
                    &target_for_handler,
                    &tool_name_for_handler,
                    arguments,
                    &handoff_metadata,
                )
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
                            "description": "Input or handoff summary for the target agent.",
                            "minLength": 1
                        }
                    },
                    "required": ["input"],
                    "additionalProperties": false
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
        metadata: std::collections::BTreeMap::new(),
    }
}

pub struct HandoffBuilder {
    target: Arc<Agent>,
    description: Option<String>,
    tool_name: Option<String>,
    metadata: std::collections::BTreeMap<String, Value>,
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

    pub fn metadata(mut self, key: impl Into<String>, value: Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }

    pub fn build(self) -> Handoff {
        let target_name = self.target.name().to_string();
        Handoff {
            target: self.target,
            description: self.description,
            tool_name: self
                .tool_name
                .unwrap_or_else(|| format!("transfer_to_{}", slugify(&target_name))),
            metadata: self.metadata,
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
    tool_name: &str,
    arguments: &ToolArguments,
    handoff_metadata: &std::collections::BTreeMap<String, Value>,
) -> ToolExecutionResult {
    let input = arguments
        .get("input")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if arguments.len() != 1 || input.is_none() {
        return ToolExecutionResult {
            tool_call_id: String::new(),
            content: json!({
                "ok": false,
                "error": "handoff requires a non-empty input string and no additional arguments",
                "error_code": "invalid_handoff_arguments",
            })
            .to_string(),
            status: ToolResultStatus::Error,
            directive: ToolDirective::Continue,
            error_code: Some("invalid_handoff_arguments".to_string()),
            metadata: std::collections::BTreeMap::new(),
            image_url: None,
            image_path: None,
        };
    }
    let input = input.expect("validated handoff input").to_string();
    let mut metadata = handoff_metadata.clone();
    metadata.insert("mode".to_string(), Value::String("handoff".to_string()));
    metadata.insert(
        "handoff_from".to_string(),
        Value::String(from_agent.to_string()),
    );
    metadata.insert(
        "handoff_to".to_string(),
        Value::String(to_agent.to_string()),
    );
    metadata.insert("handoff_input".to_string(), Value::String(input.clone()));
    metadata.insert(
        "handoff_tool_name".to_string(),
        Value::String(tool_name.to_string()),
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

fn slugify(value: &str) -> String {
    let mut normalized = String::new();
    let mut pending_separator = false;
    for character in value.chars() {
        if character.is_ascii_alphanumeric() || character == '_' {
            if pending_separator && !normalized.is_empty() && !normalized.ends_with('_') {
                normalized.push('_');
            }
            pending_separator = false;
            normalized.push(character.to_ascii_lowercase());
        } else {
            pending_separator = true;
        }
    }
    let normalized = normalized.trim_matches('_').to_string();
    if normalized.is_empty() {
        "agent".to_string()
    } else {
        normalized
    }
}
