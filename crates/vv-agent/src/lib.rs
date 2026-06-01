//! VectorVein agent runtime, SDK, CLI, tools, memory, prompt, and workspace APIs.
//!
//! This crate exposes a stable Rust library surface for building and running
//! agent workflows with built-in tool dispatch and vv-llm backed chat clients.

pub mod agent;
pub mod cli;
pub mod config;
pub mod constants;
pub mod context;
pub mod event_store;
pub mod events;
pub mod execution_mode;
pub mod guardrails;
pub mod handoffs;
pub mod integrations;
pub mod llm;
pub mod memory;
pub mod model;
pub mod model_settings;
pub mod prompt;
pub mod result;
pub mod run_config;
pub mod runner;
pub mod runtime;
pub mod sessions;
pub mod skills;
pub mod tools;
pub mod tracing;
pub mod types;
pub mod workspace;

pub use agent::{Agent, ToolUseBehavior};
pub use config::{
    apply_resolved_model_limits, build_vv_llm_from_local_settings, build_vv_llm_settings,
    decode_api_key, load_llm_settings_from_file, load_memory_summary_defaults_from_file,
    resolve_model_endpoint, ConfigError, EndpointConfig, EndpointOption, MemorySummaryDefaults,
    ResolvedModelConfig,
};
pub use context::{RunContext, ToolCallContext};
pub use event_store::{
    EventStoreError, JsonlRunEventStore, RunEventIter, RunEventReplayQuery, RunEventStore,
};
pub use events::{
    AgentErrorPayload, EventId, RunEvent, RunEventPayload, RunEventVersion, ToolStatus,
};
pub use execution_mode::ExecutionMode;
pub use guardrails::{GuardrailOutcome, InputGuardrail, OutputGuardrail};
pub use handoffs::{handoff, Handoff};
pub use llm::{
    EndpointTarget, LlmClient, LlmError, LlmRequest, LlmStreamCallback, ScriptStep,
    ScriptStepCallback, ScriptedLlmClient, VvLlmClient,
};
pub use memory::{
    sanitize_for_resume, CompactionExhaustedError, LocalSummary, MemoryManager,
    MemoryManagerConfig, SessionMemory, SessionMemoryConfig, SessionMemoryEntry,
    SessionMemoryState, SummaryCallback,
};
pub use model::{ModelError, ModelProvider, ModelRef, ScriptedModelProvider, VvLlmModelProvider};
pub use model_settings::{ModelSettings, ResponseFormat, RetryPolicy, ToolChoice};
pub use result::{RunResult, RunState};
pub use run_config::RunConfig;
pub use runner::{NormalizedInput, RunEventStream, Runner};
pub use runtime::backends::{
    run_checkpointed_cycle, CycleDispatchResult, CycleDispatcher, DistributedBackend,
    InlineBackend, RuntimeExecutionBackend, RuntimeRecipe, ThreadBackend,
};
pub use runtime::background_sessions::{
    background_session_manager, BackgroundSessionAdoptOptions, BackgroundSessionListener,
    BackgroundSessionManager, BackgroundSessionStartOptions, BackgroundSessionSubscription,
};
pub use runtime::shell::{
    build_shell_invocation, prepare_shell_execution, resolve_shell_invocation,
    PreparedShellCommand, ShellInvocation,
};
pub use runtime::state::{Checkpoint, InMemoryStateStore, StateStore};
pub use runtime::stores::redis::RedisStateStore;
pub use runtime::stores::sqlite::SqliteStateStore;
pub use runtime::sub_task_manager::{
    ManagedSubTask, ManagedSubTaskSnapshot, SubTaskManager, SubTaskSessionAttachment,
};
pub use runtime::{
    _register_sub_agent_session, _unregister_sub_agent_session, continue_sub_agent_session,
    get_sub_agent_session, register_sub_agent_session, steer_sub_agent_session,
    sub_agent_session_registry, subscribe_sub_agent_session, unregister_sub_agent_session,
    AfterLlmEvent, AfterToolCallEvent, AgentRuntime, BeforeCycleMessageProvider, BeforeLlmEvent,
    BeforeLlmPatch, BeforeMemoryCompactEvent, BeforeToolCallEvent, BeforeToolCallPatch,
    CancellationToken, CancelledError, CycleRunRequest, CycleRunner, ExecutionContext,
    InterruptionMessageProvider, RuntimeEventHandler, RuntimeHook, RuntimeHookManager,
    RuntimeRunControls, StreamCallback, SubAgentSession, SubAgentSessionListener,
    SubAgentSessionRegistry, SubAgentSessionUnsubscribe, ToolCallRunner, ToolRunOutcome,
    ToolRunRequest, MAX_PROMPT_TOO_LONG_RETRIES, MAX_PTL_RETRIES,
};
pub use sessions::{
    session_store_conformance, MemorySession, Session, SessionItem, SessionStore,
    SqliteSessionStore,
};
pub use tools::{
    build_default_registry, dispatch_tool_call, AgentTool, AgentToolBuilder, ApprovalDecision,
    ApprovalPolicy, BackgroundAgentTask, BackgroundAgentTaskBuilder, BackgroundAgentTaskHandle,
    BackgroundAgentTaskSnapshot, FunctionTool, StaticTool, Tool, ToolContext, ToolHandler,
    ToolNotFoundError, ToolOutput, ToolPolicy, ToolRegistry, ToolSpec,
};
pub use tracing::{JsonlTraceExporter, Span, TraceSink};
pub use types::{
    AgentResult, AgentStatus, AgentTask, CycleRecord, CycleStatus, LLMResponse, Message,
    MessageRole, NoToolPolicy, SubAgentConfig, SubTaskOutcome, SubTaskRequest, TaskTokenUsage,
    TokenUsage, ToolCall, ToolDirective, ToolExecutionResult, ToolResultStatus,
};
pub use workspace::{
    FileInfo, LocalWorkspaceBackend, MemoryWorkspaceBackend, S3WorkspaceBackend, S3WorkspaceConfig,
    WorkspaceBackend,
};
