//! Rust implementation surface for the Python `vv_agent` package.
//!
//! The first Rust milestone keeps module names and top-level exports aligned
//! with `v-agent/src/vv_agent/__init__.py` so downstream Rust callers can start
//! using a stable library API while deeper runtime parity is filled in module by
//! module.

pub mod background_sessions;
pub mod cli;
pub mod config;
pub mod constants;
pub mod integrations;
pub mod llm;
pub mod memory;
pub mod processes;
pub mod prompt;
pub mod runtime;
pub mod sdk;
pub mod skills;
pub mod tools;
pub mod types;
pub mod workspace;

pub use config::{
    build_openai_llm_from_local_settings, load_llm_settings_from_file, resolve_model_endpoint,
    ConfigError, EndpointConfig, EndpointOption, ResolvedModelConfig,
};
pub use llm::{EndpointTarget, LlmClient, LlmError, LlmRequest, ScriptedLlmClient, VvLlmClient};
pub use runtime::AgentRuntime;
pub use sdk::{
    create_agent_session, query, run, AgentDefinition, AgentResourceLoader, AgentRun,
    AgentSDKClient, AgentSDKOptions, AgentSession, AgentSessionState,
};
pub use tools::{build_default_registry, ToolContext, ToolHandler, ToolRegistry, ToolSpec};
pub use types::{
    AgentResult, AgentStatus, AgentTask, CycleRecord, CycleStatus, LLMResponse, Message,
    MessageRole, NoToolPolicy, SubAgentConfig, SubTaskOutcome, SubTaskRequest, TaskTokenUsage,
    TokenUsage, ToolCall, ToolDirective, ToolExecutionResult, ToolResultStatus,
};
