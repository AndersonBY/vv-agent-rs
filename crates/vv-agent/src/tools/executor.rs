use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use crate::tools::{ToolContext, ToolSpec};
use crate::types::{ToolCall, ToolExecutionResult};

pub type ToolFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ToolError>> + Send + 'a>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolExposure {
    Direct,
    Deferred,
    DirectModelOnly,
    Hidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalRequirement {
    NotRequired,
    Required,
    Provider,
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
    fn exposure(&self) -> ToolExposure {
        ToolExposure::Direct
    }
    fn supports_parallel(&self) -> bool {
        false
    }
    fn timeout(&self) -> Option<Duration> {
        None
    }
    fn spec(&self, _ctx: &ToolSpecContext) -> Result<ToolSpec, ToolError>;
    fn approval_requirement(
        &self,
        _call: &ToolCall,
        _ctx: &ToolRunContext<'_>,
    ) -> ApprovalRequirement {
        ApprovalRequirement::Provider
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
}

impl ToolSpecExecutor {
    pub fn new(spec: ToolSpec) -> Self {
        Self {
            spec,
            exposure: ToolExposure::Direct,
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

    fn exposure(&self) -> ToolExposure {
        self.exposure
    }

    fn spec(&self, _ctx: &ToolSpecContext) -> Result<ToolSpec, ToolError> {
        Ok(self.spec.clone())
    }

    fn run<'a>(
        &'a self,
        call: ToolCall,
        ctx: ToolRunContext<'a>,
    ) -> ToolFuture<'a, ToolExecutionResult> {
        Box::pin(async move {
            let mut result = (self.spec.handler)(ctx.context, &call.arguments);
            if result.tool_call_id.trim().is_empty() || result.tool_call_id == "pending" {
                result.tool_call_id = call.id;
            }
            Ok(result)
        })
    }
}
