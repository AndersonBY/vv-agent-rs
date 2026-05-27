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
pub mod sub_agent_sessions;
pub mod sub_task_manager;
pub mod tools;
pub mod types;
pub mod workspace;

pub use background_sessions::{
    background_session_manager, BackgroundSessionListener, BackgroundSessionManager,
    BackgroundSessionSubscription,
};
pub use config::{
    apply_resolved_model_limits, build_openai_llm_from_local_settings,
    build_vv_llm_from_local_settings, build_vv_llm_settings, decode_api_key,
    load_llm_settings_from_file, load_memory_summary_defaults_from_file, resolve_model_endpoint,
    ConfigError, EndpointConfig, EndpointOption, MemorySummaryDefaults, ResolvedModelConfig,
};
pub use llm::{
    EndpointTarget, LlmClient, LlmError, LlmRequest, LlmStreamCallback, ScriptedLlmClient,
    VvLlmClient,
};
pub use memory::{
    sanitize_for_resume, CompactionExhaustedError, LocalSummary, MemoryManager,
    MemoryManagerConfig, SessionMemory, SessionMemoryConfig, SessionMemoryEntry,
    SessionMemoryState,
};
pub use runtime::backends::{
    run_checkpointed_cycle, CeleryBackend, CycleTaskDispatchResult, CycleTaskDispatcher,
    InlineBackend, RuntimeExecutionBackend, RuntimeRecipe, ThreadBackend,
};
pub use runtime::shell::{
    prepare_shell_execution, resolve_shell_invocation, PreparedShellCommand, ShellInvocation,
};
pub use runtime::state::{Checkpoint, InMemoryStateStore, StateStore};
pub use runtime::stores::redis::RedisStateStore;
pub use runtime::stores::sqlite::SqliteStateStore;
pub use runtime::{
    AfterLlmEvent, AfterToolCallEvent, AgentRuntime, BeforeCycleMessageProvider, BeforeLlmEvent,
    BeforeLlmPatch, BeforeMemoryCompactEvent, BeforeToolCallEvent, BeforeToolCallPatch,
    CancellationToken, ExecutionContext, InterruptionMessageProvider, RuntimeEventHandler,
    RuntimeHook, RuntimeHookManager, RuntimeRunControls, StreamCallback,
};
pub use sdk::{
    create_agent_session, create_agent_session_with_id, query, run, AgentDefinition,
    AgentResourceLoader, AgentRun, AgentSDKClient, AgentSDKOptions, AgentSession,
    AgentSessionRunRequest, AgentSessionState, LlmBuilder, SdkLlmClient, SessionCancellationHandle,
    SessionEventHandler, SessionListenerId, SessionSteeringHandle,
};
pub use sub_agent_sessions::{
    continue_sub_agent_session, get_sub_agent_session, register_sub_agent_session,
    steer_sub_agent_session, sub_agent_session_registry, subscribe_sub_agent_session,
    unregister_sub_agent_session, SubAgentSession, SubAgentSessionListener,
    SubAgentSessionRegistry, SubAgentSessionUnsubscribe,
};
pub use sub_task_manager::SubTaskManager;
pub use tools::{build_default_registry, ToolContext, ToolHandler, ToolRegistry, ToolSpec};
pub use types::{
    AgentResult, AgentStatus, AgentTask, CycleRecord, CycleStatus, LLMResponse, Message,
    MessageRole, NoToolPolicy, SubAgentConfig, SubTaskOutcome, SubTaskRequest, TaskTokenUsage,
    TokenUsage, ToolCall, ToolDirective, ToolExecutionResult, ToolResultStatus,
};
pub use workspace::{
    FileInfo, LocalWorkspaceBackend, MemoryWorkspaceBackend, S3WorkspaceBackend, S3WorkspaceConfig,
    WorkspaceBackend,
};
