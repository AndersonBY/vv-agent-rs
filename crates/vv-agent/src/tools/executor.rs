use std::any::Any;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use crate::context::RunContext;
use crate::tools::{ToolContext, ToolMetadata, ToolSpec};
use crate::types::{Metadata, ToolArguments, ToolCall, ToolExecutionResult};

pub type ToolFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ToolError>> + Send + 'a>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolExposure {
    /// Model-visible and executable through the runtime.
    Direct,
    /// Reserved for deferred discovery. It currently has the same visibility and execution
    /// semantics as `Direct` under the shared SDK contract.
    Deferred,
    /// Reserved for model-origin-only invocation. It currently has the same visibility and
    /// execution semantics as `Direct` under the shared SDK contract.
    DirectModelOnly,
    /// Not model-visible, but still available for explicit runtime invocation.
    Hidden,
}

#[derive(Clone, Default)]
pub struct ToolEnablementContext {
    pub run: RunContext,
    pub app_state: Option<Arc<dyn Any + Send + Sync>>,
}

impl ToolEnablementContext {
    pub fn new(run: RunContext) -> Self {
        Self {
            run,
            app_state: None,
        }
    }

    pub fn with_app_state(mut self, app_state: Option<Arc<dyn Any + Send + Sync>>) -> Self {
        self.app_state = app_state;
        self
    }

    pub fn app_state<T: Send + Sync + 'static>(&self) -> Option<&T> {
        self.app_state.as_ref()?.downcast_ref::<T>()
    }
}

pub type ToolEnablementPredicate =
    Arc<dyn Fn(&ToolEnablementContext) -> bool + Send + Sync + 'static>;

#[derive(Clone)]
pub enum ToolEnablementRule {
    Static(bool),
    Predicate(ToolEnablementPredicate),
}

impl ToolEnablementRule {
    pub fn predicate<F>(predicate: F) -> Self
    where
        F: Fn(&ToolEnablementContext) -> bool + Send + Sync + 'static,
    {
        Self::Predicate(Arc::new(predicate))
    }

    pub fn is_enabled(&self, context: &ToolEnablementContext) -> bool {
        match self {
            Self::Static(enabled) => *enabled,
            Self::Predicate(predicate) => predicate(context),
        }
    }
}

impl Default for ToolEnablementRule {
    fn default() -> Self {
        Self::Static(true)
    }
}

impl From<bool> for ToolEnablementRule {
    fn from(enabled: bool) -> Self {
        Self::Static(enabled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalRequirement {
    NotRequired,
    Required,
    Provider,
}

pub type ApprovalPredicate =
    Arc<dyn Fn(&ToolContext, &ToolArguments) -> bool + Send + Sync + 'static>;

#[derive(Clone)]
pub enum ToolApprovalRule {
    Static(ApprovalRequirement),
    Predicate(ApprovalPredicate),
}

impl ToolApprovalRule {
    pub fn predicate<F>(predicate: F) -> Self
    where
        F: Fn(&ToolContext, &ToolArguments) -> bool + Send + Sync + 'static,
    {
        Self::Predicate(Arc::new(predicate))
    }

    pub fn requirement(
        &self,
        context: &ToolContext,
        arguments: &ToolArguments,
    ) -> ApprovalRequirement {
        match self {
            Self::Static(requirement) => *requirement,
            Self::Predicate(predicate) => {
                if predicate(context, arguments) {
                    ApprovalRequirement::Required
                } else {
                    ApprovalRequirement::NotRequired
                }
            }
        }
    }
}

impl Default for ToolApprovalRule {
    fn default() -> Self {
        Self::Static(ApprovalRequirement::NotRequired)
    }
}

impl From<bool> for ToolApprovalRule {
    fn from(needs_approval: bool) -> Self {
        Self::Static(if needs_approval {
            ApprovalRequirement::Required
        } else {
            ApprovalRequirement::NotRequired
        })
    }
}

impl From<ApprovalRequirement> for ToolApprovalRule {
    fn from(requirement: ApprovalRequirement) -> Self {
        Self::Static(requirement)
    }
}

#[derive(Debug, Clone, Default)]
pub struct ToolSpecContext;

pub struct ToolRunContext<'a> {
    pub context: &'a mut ToolContext,
}

impl<'a> ToolRunContext<'a> {
    pub fn new(context: &'a mut ToolContext) -> Self {
        Self { context }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolError {
    message: String,
}

impl ToolError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ToolError {}

pub trait ToolExecutor: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn metadata(&self) -> &Metadata {
        static EMPTY_METADATA: std::sync::OnceLock<Metadata> = std::sync::OnceLock::new();
        EMPTY_METADATA.get_or_init(Metadata::new)
    }
    fn exposure(&self) -> ToolExposure {
        ToolExposure::Direct
    }
    fn timeout(&self) -> Option<Duration> {
        None
    }
    fn tool_metadata(&self) -> Option<&ToolMetadata> {
        None
    }
    fn spec(&self, _ctx: &ToolSpecContext) -> Result<ToolSpec, ToolError>;
    fn approval_requirement(
        &self,
        _call: &ToolCall,
        _ctx: &ToolRunContext<'_>,
    ) -> ApprovalRequirement {
        ApprovalRequirement::NotRequired
    }
    fn validate_arguments(
        &self,
        call: &ToolCall,
    ) -> Result<Option<ToolExecutionResult>, ToolError> {
        let spec = self.spec(&ToolSpecContext)?;
        let schema = crate::tools::argument_validation::close_object_schemas(&spec.schema);
        let validator = crate::tools::argument_validation::validator_for_tool_schema(&schema)
            .map_err(ToolError::new)?;
        Ok(crate::tools::argument_validation::invalid_tool_arguments_result(&validator, call))
    }
    fn run<'a>(
        &'a self,
        call: ToolCall,
        ctx: ToolRunContext<'a>,
    ) -> ToolFuture<'a, ToolExecutionResult>;
}

#[derive(Clone)]
pub struct ToolSpecExecutor {
    spec: ToolSpec,
    exposure: ToolExposure,
    argument_validator: Result<jsonschema::Validator, String>,
}

impl ToolSpecExecutor {
    pub fn new(mut spec: ToolSpec) -> Self {
        spec.schema = crate::tools::argument_validation::close_object_schemas(&spec.schema);
        let exposure = spec.exposure;
        let argument_validator =
            crate::tools::argument_validation::validator_for_tool_schema(&spec.schema);
        Self {
            spec,
            exposure,
            argument_validator,
        }
    }

    pub fn with_exposure(mut self, exposure: ToolExposure) -> Self {
        self.exposure = exposure;
        self
    }

    pub fn into_arc(self) -> Arc<dyn ToolExecutor> {
        Arc::new(self)
    }
}

impl ToolExecutor for ToolSpecExecutor {
    fn name(&self) -> &str {
        &self.spec.name
    }

    fn description(&self) -> &str {
        &self.spec.description
    }

    fn metadata(&self) -> &Metadata {
        &self.spec.metadata
    }

    fn exposure(&self) -> ToolExposure {
        self.exposure
    }

    fn timeout(&self) -> Option<Duration> {
        self.spec.timeout
    }

    fn tool_metadata(&self) -> Option<&ToolMetadata> {
        self.spec.tool_metadata.as_ref()
    }

    fn spec(&self, _ctx: &ToolSpecContext) -> Result<ToolSpec, ToolError> {
        Ok(self.spec.clone())
    }

    fn approval_requirement(
        &self,
        call: &ToolCall,
        ctx: &ToolRunContext<'_>,
    ) -> ApprovalRequirement {
        self.spec.approval.requirement(ctx.context, &call.arguments)
    }

    fn validate_arguments(
        &self,
        call: &ToolCall,
    ) -> Result<Option<ToolExecutionResult>, ToolError> {
        let validator = self
            .argument_validator
            .as_ref()
            .map_err(|error| ToolError::new(error.clone()))?;
        Ok(crate::tools::argument_validation::invalid_tool_arguments_result(validator, call))
    }

    fn run<'a>(
        &'a self,
        call: ToolCall,
        ctx: ToolRunContext<'a>,
    ) -> ToolFuture<'a, ToolExecutionResult> {
        let handler = self.spec.handler.clone();
        Box::pin(async move {
            if let Some(result) = self.validate_arguments(&call)? {
                return Ok(result);
            }
            ctx.context.begin_tool_call(&call);
            if self.spec.timeout.is_none() {
                let mut result = handler(ctx.context, &call.arguments);
                if result.tool_call_id.trim().is_empty() || result.tool_call_id == "pending" {
                    result.tool_call_id = call.id;
                }
                return Ok(result);
            }

            let arguments = call.arguments.clone();
            let mut isolated_context = ctx.context.clone();
            let (mut result, updated_context) = tokio::task::spawn_blocking(move || {
                let result = handler(&mut isolated_context, &arguments);
                (result, isolated_context)
            })
            .await
            .map_err(|error| ToolError::new(format!("tool task failed: {error}")))?;
            *ctx.context = updated_context;
            if result.tool_call_id.trim().is_empty() || result.tool_call_id == "pending" {
                result.tool_call_id = call.id;
            }
            Ok(result)
        })
    }
}
