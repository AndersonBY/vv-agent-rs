use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::json;

use crate::tools::{ToolContext, ToolExecutor, ToolRunContext};
use crate::types::{ToolCall, ToolDirective, ToolExecutionResult, ToolResultStatus};

#[derive(Clone, Default)]
pub struct ToolRunOptions {
    allowed_tools: Option<Vec<String>>,
    disallowed_tools: Vec<String>,
}

impl ToolRunOptions {
    pub fn allow_only(mut self, tools: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.allowed_tools = Some(tools.into_iter().map(Into::into).collect());
        self
    }

    pub fn disallow(mut self, tool: impl Into<String>) -> Self {
        self.disallowed_tools.push(tool.into());
        self
    }
}

#[derive(Clone, Default)]
pub struct ToolOrchestrator {
    tools: BTreeMap<String, Arc<dyn ToolExecutor>>,
}

impl ToolOrchestrator {
    pub fn from_tools(tools: Vec<Arc<dyn ToolExecutor>>) -> Self {
        let tools = tools
            .into_iter()
            .map(|tool| (tool.name().to_string(), tool))
            .collect();
        Self { tools }
    }

    pub async fn run_one(
        &self,
        call: ToolCall,
        context: &mut ToolContext,
        options: ToolRunOptions,
    ) -> Result<ToolExecutionResult, crate::tools::ToolError> {
        if let Some(allowed) = options.allowed_tools.as_ref() {
            if !allowed.iter().any(|tool| tool == &call.name) {
                return Ok(tool_error(
                    &call,
                    "tool_not_allowed",
                    "Tool is not in the allowed tool list.",
                ));
            }
        }
        if options
            .disallowed_tools
            .iter()
            .any(|tool| tool == &call.name)
        {
            return Ok(tool_error(
                &call,
                "tool_disallowed",
                "Tool is disallowed by policy.",
            ));
        }

        let Some(tool) = self.tools.get(&call.name) else {
            return Ok(tool_error(
                &call,
                "tool_not_found",
                format!("Unknown tool: {}", call.name),
            ));
        };

        let mut result = tool.run(call.clone(), ToolRunContext::new(context)).await?;
        if result.tool_call_id.trim().is_empty() || result.tool_call_id == "pending" {
            result.tool_call_id = call.id;
        }
        Ok(result)
    }
}

fn tool_error(
    call: &ToolCall,
    error_code: &str,
    message: impl Into<String>,
) -> ToolExecutionResult {
    let error_code = error_code.to_string();
    ToolExecutionResult {
        tool_call_id: call.id.clone(),
        content: json!({
            "ok": false,
            "error": message.into(),
            "error_code": error_code,
            "tool_name": call.name,
        })
        .to_string(),
        status: ToolResultStatus::Error,
        directive: ToolDirective::Continue,
        error_code: Some(error_code),
        metadata: BTreeMap::new(),
        image_url: None,
        image_path: None,
    }
}
