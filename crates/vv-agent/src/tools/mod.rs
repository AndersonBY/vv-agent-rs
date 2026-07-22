pub mod agent_tool;
pub(crate) mod argument_validation;
pub mod background_agent_task;
pub mod base;
pub mod builtins;
pub(crate) mod common;
pub mod dispatcher;
pub mod executor;
pub mod function;
pub mod handlers;
pub mod metadata;
pub mod orchestrator;
pub mod outputs;
pub mod policy;
pub mod public_tool;
pub mod registry;
pub mod schemas;

pub use agent_tool::{AgentTool, AgentToolBuilder};
pub use background_agent_task::{
    BackgroundAgentTask, BackgroundAgentTaskBuilder, BackgroundAgentTaskHandle,
    BackgroundAgentTaskSnapshot,
};
pub use base::{
    SubTaskRunner, ToolContext, ToolHandler, ToolNotFoundError, ToolSpec, ToolSpecKind,
};
pub use dispatcher::dispatch_tool_call;
pub use executor::{
    ApprovalPredicate, ApprovalRequirement, ToolApprovalRule, ToolEnablementContext,
    ToolEnablementPredicate, ToolEnablementRule, ToolError, ToolExecutor, ToolExposure, ToolFuture,
    ToolRunContext, ToolSpecContext, ToolSpecExecutor,
};
pub use function::{FunctionTool, ToolErrorMapper};
pub use metadata::{ToolMetadata, ToolMetadataError, ToolSideEffect};
pub use orchestrator::{
    BeforeToolDispatch, ToolLifecycleCallback, ToolLifecycleEvent, ToolOrchestrator, ToolRunOptions,
};
pub use outputs::ToolOutput;
pub use policy::{ApprovalDecision, ApprovalPolicy, CanUseToolPredicate, ToolPolicy};
pub use public_tool::{StaticTool, Tool};
pub use registry::{build_default_registry, ToolRegistry};
