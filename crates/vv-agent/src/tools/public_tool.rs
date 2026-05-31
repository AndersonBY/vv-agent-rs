use serde_json::Value;

use crate::tools::{ToolHandler, ToolSpec};
use crate::types::ToolExecutionResult;

pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> &Value;

    fn strict_schema(&self) -> bool {
        true
    }

    fn as_tool_spec(&self) -> ToolSpec;
}

#[derive(Clone)]
pub struct StaticTool {
    name: String,
    description: String,
    parameters_schema: Value,
    handler: ToolHandler,
}

impl StaticTool {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters_schema: Value,
        handler: ToolHandler,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters_schema,
            handler,
        }
    }
}

impl Tool for StaticTool {
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
        let mut spec = ToolSpec::new(
            self.name.clone(),
            self.description.clone(),
            self.handler.clone(),
        );
        spec.schema = serde_json::json!({
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

impl<F> From<(String, String, Value, F)> for StaticTool
where
    F: Fn(&mut crate::tools::ToolContext, &crate::types::ToolArguments) -> ToolExecutionResult
        + Send
        + Sync
        + 'static,
{
    fn from(value: (String, String, Value, F)) -> Self {
        Self::new(value.0, value.1, value.2, std::sync::Arc::new(value.3))
    }
}
