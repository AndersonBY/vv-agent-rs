//! VectorVein agent runtime, SDK, CLI, tools, memory, prompt, and workspace APIs.
//!
//! This crate exposes a stable Rust library surface for building and running
//! agent workflows with built-in tool dispatch and vv-llm backed chat clients.

pub mod cli;
pub mod config;
pub mod constants;
pub mod integrations;
pub mod llm;
pub mod memory;
pub mod prompt;
pub mod runtime;
pub mod sdk;
pub mod skills;
pub mod tools;
pub mod types;
pub mod workspace;

pub use config::{
    apply_resolved_model_limits, build_openai_llm_from_local_settings,
    build_vv_llm_from_local_settings, build_vv_llm_settings, decode_api_key,
    load_llm_settings_from_file, load_memory_summary_defaults_from_file, resolve_model_endpoint,
    ConfigError, EndpointConfig, EndpointOption, MemorySummaryDefaults, ResolvedModelConfig,
};
pub use llm::{
    EndpointTarget, LLMClient, LlmClient, LlmError, LlmRequest, LlmStreamCallback, ScriptStep,
    ScriptStepCallback, ScriptedLLM, ScriptedLlmClient, VVLlmClient, VvLlmClient,
};
pub use memory::{
    sanitize_for_resume, CompactionExhaustedError, LocalSummary, MemoryManager,
    MemoryManagerConfig, SessionMemory, SessionMemoryConfig, SessionMemoryEntry,
    SessionMemoryState, SummaryCallback,
};
pub use runtime::backends::{
    run_checkpointed_cycle, CeleryBackend, CycleTaskDispatchResult, CycleTaskDispatcher,
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
    AfterLLMEvent, AfterLlmEvent, AfterToolCallEvent, AgentRuntime, BaseRuntimeHook,
    BeforeCycleMessageProvider, BeforeLLMEvent, BeforeLLMPatch, BeforeLlmEvent, BeforeLlmPatch,
    BeforeMemoryCompactEvent, BeforeToolCallEvent, BeforeToolCallPatch, CancellationToken,
    CancelledError, CycleRunRequest, CycleRunner, ExecutionBackend, ExecutionContext,
    InterruptionMessageProvider, RuntimeEventHandler, RuntimeHook, RuntimeHookManager,
    RuntimeRunControls, StreamCallback, SubAgentSession, SubAgentSessionListener,
    SubAgentSessionRegistry, SubAgentSessionUnsubscribe, ToolCallRunner, ToolRunOutcome,
    ToolRunRequest, MAX_PROMPT_TOO_LONG_RETRIES, MAX_PTL_RETRIES,
};
pub use sdk::{
    create_agent_session, create_agent_session_with_id, create_agent_session_with_id_and_workspace,
    create_agent_session_with_id_and_workspace_and_shared_state,
    create_agent_session_with_shared_state, create_agent_session_with_workspace,
    create_agent_session_with_workspace_and_shared_state, query, query_with_options_and_agent,
    query_with_options_and_agent_in_workspace,
    query_with_options_and_agent_in_workspace_with_require_completed,
    query_with_options_and_agent_request,
    query_with_options_and_agent_request_with_require_completed,
    query_with_options_and_agent_with_require_completed, run, run_with_options_and_agent,
    run_with_options_and_agent_in_workspace, run_with_options_and_agent_request, AgentDefinition,
    AgentResourceLoader, AgentRun, AgentSDKClient, AgentSDKOptions, AgentSession,
    AgentSessionRunRequest, AgentSessionState, LlmBuilder, SdkLlmClient, SessionCancellationHandle,
    SessionEventHandler, SessionListenerId, SessionSteeringHandle, ToolRegistryFactory,
};
pub use tools::{
    build_default_registry, dispatch_tool_call, ToolContext, ToolHandler, ToolNotFoundError,
    ToolRegistry, ToolSpec,
};
pub use types::{
    AgentResult, AgentStatus, AgentTask, CycleRecord, CycleStatus, LLMResponse, Message,
    MessageRole, NoToolPolicy, SubAgentConfig, SubTaskOutcome, SubTaskRequest, TaskTokenUsage,
    TokenUsage, ToolCall, ToolDirective, ToolExecutionResult, ToolResultStatus,
};
pub use workspace::{
    FileInfo, LocalWorkspaceBackend, MemoryWorkspaceBackend, S3WorkspaceBackend, S3WorkspaceConfig,
    WorkspaceBackend,
};
