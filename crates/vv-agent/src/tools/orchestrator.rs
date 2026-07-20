use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;
use std::time::Instant;

use serde_json::json;

use crate::tools::{
    ApprovalPolicy, ApprovalRequirement, CanUseToolPredicate, ToolContext, ToolExecutor,
    ToolMetadata, ToolPolicy, ToolRunContext,
};
use crate::types::{Metadata, ToolCall, ToolDirective, ToolExecutionResult, ToolResultStatus};

pub type BeforeToolDispatch = Arc<
    dyn Fn(&ToolCall, &mut ToolContext) -> Result<(), crate::tools::ToolError>
        + Send
        + Sync
        + 'static,
>;

pub type ToolLifecycleCallback = Arc<dyn Fn(ToolLifecycleEvent) + Send + Sync + 'static>;

#[derive(Debug, Clone)]
pub enum ToolLifecycleEvent {
    Planned {
        call: ToolCall,
        tool_metadata: Option<ToolMetadata>,
    },
    Started {
        call: ToolCall,
        tool_metadata: Option<ToolMetadata>,
    },
    Completed {
        call: ToolCall,
        result: ToolExecutionResult,
        execution_started: bool,
        duration_ms: Option<u64>,
        tool_metadata: Option<ToolMetadata>,
    },
}

pub(crate) struct DeferredToolExecution {
    result: ToolExecutionResult,
    lifecycle: Option<DeferredToolLifecycle>,
}

struct DeferredToolLifecycle {
    call: ToolCall,
    execution_started: bool,
    duration_ms: Option<u64>,
    tool_metadata: Option<ToolMetadata>,
    callback: Option<ToolLifecycleCallback>,
}

impl DeferredToolExecution {
    pub(crate) fn without_lifecycle(result: ToolExecutionResult) -> Self {
        Self {
            result,
            lifecycle: None,
        }
    }

    fn with_lifecycle(
        call: ToolCall,
        result: ToolExecutionResult,
        execution_started: bool,
        duration_ms: Option<u64>,
        tool_metadata: Option<ToolMetadata>,
        callback: Option<ToolLifecycleCallback>,
    ) -> Self {
        Self {
            result,
            lifecycle: Some(DeferredToolLifecycle {
                call,
                execution_started,
                duration_ms,
                tool_metadata,
                callback,
            }),
        }
    }

    pub(crate) fn result(&self) -> &ToolExecutionResult {
        &self.result
    }

    pub(crate) fn replace_result(&mut self, result: ToolExecutionResult) {
        self.result = result;
    }

    pub(crate) fn execution_started(&self) -> bool {
        self.lifecycle
            .as_ref()
            .is_some_and(|lifecycle| lifecycle.execution_started)
    }

    pub(crate) fn complete(self) -> ToolExecutionResult {
        if let Some(lifecycle) = self.lifecycle {
            emit_completed(
                &lifecycle.call,
                &self.result,
                lifecycle.execution_started,
                lifecycle.duration_ms,
                lifecycle.tool_metadata,
                lifecycle.callback.as_ref(),
            );
        }
        self.result
    }
}

#[derive(Clone, Default)]
pub struct ToolRunOptions {
    planned_tools: Option<Vec<String>>,
    allowed_tools: Option<Vec<String>>,
    disallowed_tools: Vec<String>,
    can_use_tool: Option<CanUseToolPredicate>,
    approval: ApprovalPolicy,
    denied_side_effects: Vec<crate::tools::ToolSideEffect>,
    denied_capability_tags: Vec<String>,
    deny_terminal_tools: bool,
    denied_cost_dimensions: Vec<String>,
    idempotency_key: Option<String>,
    before_dispatch: Option<BeforeToolDispatch>,
    lifecycle_callback: Option<ToolLifecycleCallback>,
}

impl ToolRunOptions {
    pub fn from_policy(policy: &ToolPolicy) -> Self {
        Self {
            planned_tools: None,
            allowed_tools: policy.allowed_tools.clone(),
            disallowed_tools: policy.disallowed_tools.clone(),
            can_use_tool: policy.can_use_tool.clone(),
            approval: policy.approval,
            denied_side_effects: policy.denied_side_effects.clone(),
            denied_capability_tags: policy.denied_capability_tags.clone(),
            deny_terminal_tools: policy.deny_terminal_tools,
            denied_cost_dimensions: policy.denied_cost_dimensions.clone(),
            idempotency_key: None,
            before_dispatch: None,
            lifecycle_callback: None,
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

    pub fn lifecycle_callback(mut self, callback: ToolLifecycleCallback) -> Self {
        self.lifecycle_callback = Some(callback);
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
        self.run_one_with_approval_and_metadata_deferred(call, context, options, approval)
            .await
            .map(DeferredToolExecution::complete)
    }

    pub(crate) async fn run_one_with_approval_and_metadata_deferred<F>(
        &self,
        call: ToolCall,
        context: &mut ToolContext,
        options: ToolRunOptions,
        approval: F,
    ) -> Result<DeferredToolExecution, crate::tools::ToolError>
    where
        F: FnOnce(
            &ToolCall,
            ApprovalRequirement,
            &ToolContext,
            &Metadata,
        ) -> Option<ToolExecutionResult>,
    {
        if let Some(result) = crate::tools::dispatcher::argument_error_result(&call) {
            return Ok(DeferredToolExecution::without_lifecycle(result));
        }

        let tool = self.tools.get(&call.name);
        let tool_metadata = tool.and_then(|tool| tool.tool_metadata()).cloned();
        emit_lifecycle(
            options.lifecycle_callback.as_ref(),
            ToolLifecycleEvent::Planned {
                call: call.clone(),
                tool_metadata: tool_metadata.clone(),
            },
        );

        if let Some(allowed) = options.allowed_tools.as_ref() {
            if !allowed.iter().any(|tool| tool == &call.name) {
                return Ok(deferred_without_execution(
                    &call,
                    policy_denial(&call, "allowed_tools"),
                    tool_metadata,
                    options.lifecycle_callback.as_ref(),
                ));
            }
        }
        if options
            .disallowed_tools
            .iter()
            .any(|tool| tool == &call.name)
        {
            return Ok(deferred_without_execution(
                &call,
                policy_denial(&call, "disallowed_tools"),
                tool_metadata,
                options.lifecycle_callback.as_ref(),
            ));
        }
        if options
            .can_use_tool
            .as_ref()
            .is_some_and(|predicate| !predicate(&call.name, &call.arguments))
        {
            return Ok(deferred_without_execution(
                &call,
                policy_denial(&call, "can_use_tool"),
                tool_metadata,
                options.lifecycle_callback.as_ref(),
            ));
        }
        if options
            .planned_tools
            .as_ref()
            .is_some_and(|planned| !planned.iter().any(|tool| tool == &call.name))
        {
            return Ok(deferred_without_execution(
                &call,
                policy_denial(&call, "planned_name"),
                tool_metadata,
                options.lifecycle_callback.as_ref(),
            ));
        }

        let Some(tool) = tool else {
            let result = tool_error(
                &call,
                "tool_not_found",
                format!("Unknown tool: {}", call.name),
                None,
            );
            return Ok(deferred_without_execution(
                &call,
                result,
                None,
                options.lifecycle_callback.as_ref(),
            ));
        };

        if let Some(policy_source) = (ToolPolicy {
            denied_side_effects: options.denied_side_effects.clone(),
            denied_capability_tags: options.denied_capability_tags.clone(),
            deny_terminal_tools: options.deny_terminal_tools,
            denied_cost_dimensions: options.denied_cost_dimensions.clone(),
            ..ToolPolicy::default()
        })
        .metadata_denial_source(tool_metadata.as_ref())
        {
            return Ok(deferred_without_execution(
                &call,
                policy_denial(&call, policy_source),
                tool_metadata,
                options.lifecycle_callback.as_ref(),
            ));
        }

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
                result.tool_call_id = call.id.clone();
            }
            return Ok(deferred_without_execution(
                &call,
                result,
                tool_metadata,
                options.lifecycle_callback.as_ref(),
            ));
        }

        if let Some(callback) = options.before_dispatch.as_ref() {
            callback(&call, context)?;
        }

        let started_at = Instant::now();
        emit_lifecycle(
            options.lifecycle_callback.as_ref(),
            ToolLifecycleEvent::Started {
                call: call.clone(),
                tool_metadata: tool_metadata.clone(),
            },
        );
        let future = tool.run(call.clone(), ToolRunContext::new(context));
        let mut result = if let Some(timeout) = tool.timeout() {
            match tokio::time::timeout(timeout, future).await {
                Ok(result) => result?,
                Err(_) => {
                    let result = crate::tools::ToolOutput::error(format!(
                        "Tool {} timed out after {} seconds.",
                        call.name,
                        timeout.as_secs_f64()
                    ))
                    .with_code("tool_timeout")
                    .retryable(true)
                    .to_result(&call.id);
                    return Ok(DeferredToolExecution::with_lifecycle(
                        call,
                        result,
                        true,
                        Some(elapsed_millis(started_at)),
                        tool_metadata,
                        options.lifecycle_callback,
                    ));
                }
            }
        } else {
            future.await?
        };
        if result.tool_call_id.trim().is_empty() || result.tool_call_id == "pending" {
            result.tool_call_id = call.id.clone();
        }
        Ok(DeferredToolExecution::with_lifecycle(
            call,
            result,
            true,
            Some(elapsed_millis(started_at)),
            tool_metadata,
            options.lifecycle_callback,
        ))
    }

    pub(crate) fn observe_result_without_execution(
        &self,
        call: ToolCall,
        result: ToolExecutionResult,
        options: &ToolRunOptions,
    ) -> DeferredToolExecution {
        if crate::tools::dispatcher::argument_error_result(&call).is_some() {
            return DeferredToolExecution::without_lifecycle(result);
        }
        let tool_metadata = self
            .tools
            .get(&call.name)
            .and_then(|tool| tool.tool_metadata())
            .cloned();
        emit_lifecycle(
            options.lifecycle_callback.as_ref(),
            ToolLifecycleEvent::Planned {
                call: call.clone(),
                tool_metadata: tool_metadata.clone(),
            },
        );
        deferred_without_execution(
            &call,
            result,
            tool_metadata,
            options.lifecycle_callback.as_ref(),
        )
    }
}

fn deferred_without_execution(
    call: &ToolCall,
    result: ToolExecutionResult,
    tool_metadata: Option<ToolMetadata>,
    callback: Option<&ToolLifecycleCallback>,
) -> DeferredToolExecution {
    DeferredToolExecution::with_lifecycle(
        call.clone(),
        result,
        false,
        None,
        tool_metadata,
        callback.cloned(),
    )
}

fn emit_completed(
    call: &ToolCall,
    result: &ToolExecutionResult,
    execution_started: bool,
    duration_ms: Option<u64>,
    tool_metadata: Option<ToolMetadata>,
    callback: Option<&ToolLifecycleCallback>,
) {
    emit_lifecycle(
        callback,
        ToolLifecycleEvent::Completed {
            call: call.clone(),
            result: result.clone(),
            execution_started,
            duration_ms,
            tool_metadata,
        },
    );
}

fn emit_lifecycle(callback: Option<&ToolLifecycleCallback>, event: ToolLifecycleEvent) {
    let Some(callback) = callback else {
        return;
    };
    let _ = catch_unwind(AssertUnwindSafe(|| callback(event)));
}

fn elapsed_millis(started_at: Instant) -> u64 {
    const JSON_SAFE_INTEGER_MAX: u128 = (1_u128 << 53) - 1;
    started_at.elapsed().as_millis().min(JSON_SAFE_INTEGER_MAX) as u64
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
