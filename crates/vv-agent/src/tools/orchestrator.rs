use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::json;

use crate::tools::{
    ApprovalPolicy, ApprovalRequirement, CanUseToolPredicate, ToolContext, ToolExecutor,
    ToolPolicy, ToolRunContext,
};
use crate::types::{Metadata, ToolCall, ToolDirective, ToolExecutionResult, ToolResultStatus};

pub type BeforeToolDispatch = Arc<
    dyn Fn(&ToolCall, &mut ToolContext) -> Result<(), crate::tools::ToolError>
        + Send
        + Sync
        + 'static,
>;

#[derive(Clone, Default)]
pub struct ToolRunOptions {
    planned_tools: Option<Vec<String>>,
    allowed_tools: Option<Vec<String>>,
    disallowed_tools: Vec<String>,
    can_use_tool: Option<CanUseToolPredicate>,
    approval: ApprovalPolicy,
    idempotency_key: Option<String>,
    before_dispatch: Option<BeforeToolDispatch>,
}

impl ToolRunOptions {
    pub fn from_policy(policy: &ToolPolicy) -> Self {
        Self {
            planned_tools: None,
            allowed_tools: policy.allowed_tools.clone(),
            disallowed_tools: policy.disallowed_tools.clone(),
            can_use_tool: policy.can_use_tool.clone(),
            approval: policy.approval,
            idempotency_key: None,
            before_dispatch: None,
        }
    }

    pub fn planned_names(mut self, tools: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.planned_tools = Some(tools.into_iter().map(Into::into).collect());
        self
    }

    pub fn allow_only(mut self, tools: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.allowed_tools = Some(tools.into_iter().map(Into::into).collect());
        self
    }

    pub fn disallow(mut self, tool: impl Into<String>) -> Self {
        self.disallowed_tools.push(tool.into());
        self
    }

    pub fn can_use_tool<F>(mut self, predicate: F) -> Self
    where
        F: Fn(&str, &crate::types::ToolArguments) -> bool + Send + Sync + 'static,
    {
        self.can_use_tool = Some(Arc::new(predicate));
        self
    }

    pub fn idempotency_key(mut self, idempotency_key: Option<String>) -> Self {
        self.idempotency_key = idempotency_key;
        self
    }

    pub fn before_dispatch(mut self, callback: BeforeToolDispatch) -> Self {
        self.before_dispatch = Some(callback);
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
        self.run_one_with_approval(call, context, options, |_call, _requirement, _context| None)
            .await
    }

    pub(crate) async fn run_one_with_approval<F>(
        &self,
        call: ToolCall,
        context: &mut ToolContext,
        options: ToolRunOptions,
        approval: F,
    ) -> Result<ToolExecutionResult, crate::tools::ToolError>
    where
        F: FnOnce(&ToolCall, ApprovalRequirement, &ToolContext) -> Option<ToolExecutionResult>,
    {
        self.run_one_with_approval_and_metadata(
            call,
            context,
            options,
            |call, requirement, context, _metadata| approval(call, requirement, context),
        )
        .await
    }

    pub(crate) async fn run_one_with_approval_and_metadata<F>(
        &self,
        call: ToolCall,
        context: &mut ToolContext,
        options: ToolRunOptions,
        approval: F,
    ) -> Result<ToolExecutionResult, crate::tools::ToolError>
    where
        F: FnOnce(
            &ToolCall,
            ApprovalRequirement,
            &ToolContext,
            &Metadata,
        ) -> Option<ToolExecutionResult>,
    {
        if let Some(allowed) = options.allowed_tools.as_ref() {
            if !allowed.iter().any(|tool| tool == &call.name) {
                return Ok(policy_denial(&call, "allowed_tools"));
            }
        }
        if options
            .disallowed_tools
            .iter()
            .any(|tool| tool == &call.name)
        {
            return Ok(policy_denial(&call, "disallowed_tools"));
        }
        if options
            .can_use_tool
            .as_ref()
            .is_some_and(|predicate| !predicate(&call.name, &call.arguments))
        {
            return Ok(policy_denial(&call, "can_use_tool"));
        }
        if options
            .planned_tools
            .as_ref()
            .is_some_and(|planned| !planned.iter().any(|tool| tool == &call.name))
        {
            return Ok(policy_denial(&call, "planned_name"));
        }

        let Some(tool) = self.tools.get(&call.name) else {
            return Ok(tool_error(
                &call,
                "tool_not_found",
                format!("Unknown tool: {}", call.name),
                None,
            ));
        };

        context.begin_tool_call(&call);
        context.idempotency_key = options.idempotency_key.clone();
        let approval_requirement = match options.approval {
            ApprovalPolicy::Never => ApprovalRequirement::NotRequired,
            ApprovalPolicy::Always => ApprovalRequirement::Required,
            ApprovalPolicy::Default | ApprovalPolicy::OnRequest => {
                tool.approval_requirement(&call, &ToolRunContext::new(context))
            }
        };
        let approval_result = approval(&call, approval_requirement, context, tool.metadata());
        if let Some(mut result) = approval_result {
            if result.tool_call_id.trim().is_empty() || result.tool_call_id == "pending" {
                result.tool_call_id = call.id;
            }
            return Ok(result);
        }

        if let Some(callback) = options.before_dispatch.as_ref() {
            callback(&call, context)?;
        }

        let future = tool.run(call.clone(), ToolRunContext::new(context));
        let mut result = if let Some(timeout) = tool.timeout() {
            match tokio::time::timeout(timeout, future).await {
                Ok(result) => result?,
                Err(_) => {
                    return Ok(crate::tools::ToolOutput::error(format!(
                        "Tool {} timed out after {} seconds.",
                        call.name,
                        timeout.as_secs_f64()
                    ))
                    .with_code("tool_timeout")
                    .retryable(true)
                    .to_result(&call.id));
                }
            }
        } else {
            future.await?
        };
        if result.tool_call_id.trim().is_empty() || result.tool_call_id == "pending" {
            result.tool_call_id = call.id;
        }
        Ok(result)
    }
}

fn policy_denial(call: &ToolCall, policy_source: &str) -> ToolExecutionResult {
    tool_error(
        call,
        "tool_not_allowed",
        format!("Tool {} is not allowed for these arguments.", call.name),
        Some(policy_source),
    )
}

fn tool_error(
    call: &ToolCall,
    error_code: &str,
    message: impl Into<String>,
    policy_source: Option<&str>,
) -> ToolExecutionResult {
    let error_code = error_code.to_string();
    let message = message.into();
    let mut metadata = BTreeMap::new();
    if let Some(policy_source) = policy_source {
        metadata.insert("mode".to_string(), json!("permission_denied"));
        metadata.insert("policy_source".to_string(), json!(policy_source));
        metadata.insert("tool_name".to_string(), json!(call.name));
        metadata.insert("arguments".to_string(), json!(call.arguments));
        metadata.insert("message".to_string(), json!(message));
    }
    ToolExecutionResult {
        tool_call_id: call.id.clone(),
        content: json!({
            "ok": false,
            "error": message,
            "error_code": error_code,
            "tool_name": call.name,
        })
        .to_string(),
        status: ToolResultStatus::Error,
        directive: ToolDirective::Continue,
        error_code: Some(error_code),
        metadata,
        image_url: None,
        image_path: None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::*;
    use crate::tools::{FunctionTool, ToolOutput};

    #[tokio::test]
    async fn metadata_aware_approval_callback_receives_effective_requirement() {
        let tool = FunctionTool::builder("guarded")
            .metadata("risk_level", json!("high"))
            .needs_approval_if(|_context, _arguments| true)
            .handler(|_context, _arguments: Value| async { Ok(ToolOutput::text("ran")) })
            .build()
            .expect("guarded tool");
        let orchestrator = ToolOrchestrator::from_tools(vec![tool.to_executor()]);
        let mut context = ToolContext::new("./workspace");

        let result = orchestrator
            .run_one_with_approval_and_metadata(
                ToolCall::from_raw_arguments("guarded_call", "guarded", json!({})),
                &mut context,
                ToolRunOptions::default(),
                |_call, requirement, _context, metadata| {
                    assert_eq!(requirement, ApprovalRequirement::Required);
                    assert_eq!(metadata["risk_level"], json!("high"));
                    None
                },
            )
            .await
            .expect("tool result");

        assert_eq!(result.status, ToolResultStatus::Success);
    }
}
