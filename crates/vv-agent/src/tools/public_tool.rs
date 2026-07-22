use serde_json::Value;
use std::time::Duration;

use crate::tools::{
    ToolApprovalRule, ToolEnablementContext, ToolEnablementRule, ToolHandler, ToolMetadata,
    ToolSpec,
};
use crate::types::ToolExecutionResult;

pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> &Value;

    fn strict_schema(&self) -> bool {
        true
    }

    fn exposure(&self) -> crate::tools::ToolExposure {
        crate::tools::ToolExposure::Direct
    }

    fn timeout(&self) -> Option<Duration> {
        None
    }

    fn approval_rule(&self) -> ToolApprovalRule {
        ToolApprovalRule::default()
    }

    fn tool_metadata(&self) -> Option<&ToolMetadata> {
        None
    }

    fn is_enabled(&self, _context: &ToolEnablementContext) -> bool {
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
    enablement: ToolEnablementRule,
    tool_metadata: Option<ToolMetadata>,
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
            enablement: ToolEnablementRule::default(),
            tool_metadata: None,
        }
    }

    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enablement = ToolEnablementRule::Static(enabled);
        self
    }

    pub fn with_enabled_if<F>(mut self, predicate: F) -> Self
    where
        F: Fn(&ToolEnablementContext) -> bool + Send + Sync + 'static,
    {
        self.enablement = ToolEnablementRule::predicate(predicate);
        self
    }

    pub fn with_tool_metadata(mut self, tool_metadata: ToolMetadata) -> Self {
        self.tool_metadata = Some(tool_metadata);
        self
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

    fn is_enabled(&self, context: &ToolEnablementContext) -> bool {
        self.enablement.is_enabled(context)
    }

    fn tool_metadata(&self) -> Option<&ToolMetadata> {
        self.tool_metadata.as_ref()
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
                "strict": self.strict_schema(),
            }
        });
        spec.strict_schema = self.strict_schema();
        spec.exposure = self.exposure();
        spec.timeout = self.timeout();
        spec.approval = self.approval_rule();
        spec.tool_metadata = self.tool_metadata().cloned();
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
