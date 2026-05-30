use std::sync::Arc;

use serde_json::{json, Value};

use super::ToolContext;
use crate::types::{ToolArguments, ToolExecutionResult};

pub type ToolHandler =
    Arc<dyn Fn(&mut ToolContext, &ToolArguments) -> ToolExecutionResult + Send + Sync + 'static>;
pub type SubTaskRunner = Arc<
    dyn Fn(crate::types::SubTaskRequest) -> crate::types::SubTaskOutcome + Send + Sync + 'static,
>;

#[derive(Clone)]
pub struct ToolSpec {
    pub name: String,
    pub handler: ToolHandler,
    pub description: String,
    pub schema: Value,
}

impl ToolSpec {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        handler: ToolHandler,
    ) -> Self {
        let name = name.into();
        let fallback_description = description.into();
        let schema = crate::tools::schemas::schema_for(&name).unwrap_or_else(|| {
            json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": fallback_description,
                    "parameters": {"type": "object", "properties": {}, "required": []},
                }
            })
        });
        let description = schema["function"]["description"]
            .as_str()
            .unwrap_or(&fallback_description)
            .to_string();
        Self {
            schema,
            name,
            handler,
            description,
        }
    }
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("tool not found: {0}")]
pub struct ToolNotFoundError(pub String);
