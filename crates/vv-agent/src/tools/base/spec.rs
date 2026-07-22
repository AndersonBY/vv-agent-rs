use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use super::ToolContext;
use crate::tools::{ToolApprovalRule, ToolMetadata};
use crate::types::{Metadata, ToolArguments, ToolExecutionResult};

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
    pub kind: ToolSpecKind,
    pub strict_schema: bool,
    pub exposure: crate::tools::ToolExposure,
    pub timeout: Option<Duration>,
    pub approval: ToolApprovalRule,
    pub tool_metadata: Option<ToolMetadata>,
    pub metadata: Metadata,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSpecKind {
    Function,
    Agent,
    BackgroundAgent,
    Handoff,
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
                    "parameters": {
                        "type": "object",
                        "properties": {},
                        "required": [],
                        "additionalProperties": false
                    },
                }
            })
        });
        let schema = crate::tools::argument_validation::close_object_schemas(&schema);
        let description = schema["function"]["description"]
            .as_str()
            .unwrap_or(&fallback_description)
            .to_string();
        Self {
            schema,
            name,
            handler,
            description,
            kind: ToolSpecKind::Function,
            strict_schema: true,
            exposure: crate::tools::ToolExposure::Direct,
            timeout: None,
            approval: ToolApprovalRule::default(),
            tool_metadata: None,
            metadata: Metadata::new(),
        }
    }
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("tool not found: {0}")]
pub struct ToolNotFoundError(pub String);
