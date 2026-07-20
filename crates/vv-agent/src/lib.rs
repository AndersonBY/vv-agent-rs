//! VectorVein agent runtime, SDK, CLI, tools, memory, prompt, and workspace APIs.
//!
//! This crate exposes a stable Rust library surface for building and running
//! agent workflows with built-in tool dispatch and vv-llm backed chat clients.

pub mod agent;
pub mod app_server;
pub mod approval;
pub mod budget;
pub mod checkpoint;
pub mod cli;
pub mod config;
pub mod constants;
pub mod context;
pub mod context_providers;
pub mod event_store;
pub mod events;
pub mod execution_mode;
pub mod guardrails;
pub mod handoffs;
pub mod integrations;
pub mod interactive;
pub mod llm;
pub mod memory;
pub mod model;
pub mod model_settings;
pub mod prompt;
pub mod result;
pub mod run_config;
pub mod run_handle;
pub mod runner;
pub mod runtime;
pub mod sessions;
pub mod skills;
pub mod tools;
pub mod tracing;
pub mod types;
pub mod workspace;

pub use agent::{Agent, InstructionProvider, ToolUseBehavior};
pub use app_server::{
    AgentResolutionRequest, AppServerHost, AppServerHostError, DefaultAppServerHost,
    RunConfigResolutionRequest,
};
pub use approval::{
    ApprovalBroker, ApprovalError, ApprovalFuture, ApprovalProvider, ApprovalRequest,
};
pub use budget::{
    BudgetDimension, BudgetEnforcementBoundary, BudgetExhaustion, BudgetExhaustionReason,
    BudgetUnavailableDimension, BudgetUnavailableReason, BudgetUsageSnapshot, HostCost,
    HostCostMeter, RunBudgetLimits, RunBudgetLimitsBuilder, UnavailableMetricPolicy,
    MAX_WIRE_INTEGER,
};
pub use checkpoint::{
    canonical_json_bytes, event_payload_digest, model_request_digest, normalize_run_definition,
    operation_request_digest, redact_run_definition, run_definition_digest, tool_request_digest,
    validate_extension_namespace, validate_run_definition, AmbiguousModelPolicy,
    AmbiguousToolPolicy, AppendOnceResult, CheckpointConfig, CheckpointError, CheckpointExtension,
    CheckpointStatus, ClaimMode, EventCursor, IdempotentRunEventStore, InMemoryRunEventStore,
    OperationKind, OperationState, ReconciliationDecision, ReconciliationDecisionKind,
    ReconciliationError, ReconciliationProvider, ResumeObservation, ResumePolicy, ToolIdempotency,
};
pub use config::{
    apply_resolved_model_limits, build_vv_llm_from_local_settings, build_vv_llm_settings,
    decode_api_key, load_llm_settings_from_file, load_memory_summary_defaults_from_file,
    resolve_model_endpoint, ConfigError, EndpointConfig, EndpointOption, MemorySummaryDefaults,
    ResolvedModelConfig,
};
pub use context::{RunContext, ToolCallContext};
pub use context_providers::{
    assemble_context_fragments, collect_context_fragments, ContextBundle, ContextError,
    ContextFragment, ContextProvider, ContextRequest, ContextSection,
};
pub use event_store::{
    EventStoreError, JsonlRunEventStore, RunEventIter, RunEventReplayQuery, RunEventStore,
};
pub use events::{
    AgentErrorPayload, EventId, RunEvent, RunEventPayload, RunEventVersion, ToolStatus,
};
pub use execution_mode::ExecutionMode;
pub use guardrails::{GuardrailOutcome, InputGuardrail, OutputGuardrail};
pub use handoffs::{handoff, Handoff};
pub use interactive::{
    create_interactive_session, InteractiveAgentClient, InteractiveSession,
    InteractiveSessionError, InteractiveSessionEvent, InteractiveSessionOptions,
    InteractiveSessionState,
};
pub use llm::{
    EndpointTarget, LlmClient, LlmError, LlmRequest, LlmStreamCallback, ScriptStep,
    ScriptStepCallback, ScriptedLlmClient, VvLlmClient,
};
pub use memory::{
    sanitize_for_resume, CompactionExhaustedError, LocalSummary, MemoryError, MemoryFuture,
    MemoryManager, MemoryManagerConfig, MemoryProvider, MemoryProviderResult, MemorySaveRequest,
    MemorySaveResult, MemorySearchRequest, MemorySearchResult, SessionMemory, SessionMemoryConfig,
    SessionMemoryEntry, SessionMemoryState, SummaryCallback,
};
pub use model::{ModelError, ModelProvider, ModelRef, ScriptedModelProvider, VvLlmModelProvider};
pub use model_settings::{ModelSettings, ResponseFormat, RetryPolicy, RetrySettings, ToolChoice};
pub use result::{ApprovalSnapshot, FinalOutputError, RunResult, RunState};
pub use run_config::{RunConfig, ToolRegistryFactory};
pub use run_handle::{RunHandle, RunHandleState, RunHandleStatus};
pub use runner::{NormalizedInput, RunEventStream, Runner};
pub use runtime::backends::{
    run_checkpointed_cycle, CapabilityRef, CycleDispatchResult, CycleDispatcher,
    DistributedBackend, DistributedCapabilities, DistributedCapabilityError,
    DistributedCapabilityRegistry, DistributedCycleWorker, DistributedRunEnvelope,
    DistributedToolPolicy, InlineBackend, ResolvedDistributedCapabilities, RuntimeExecutionBackend,
    RuntimeRecipe, ThreadBackend, ToolsetRef,
};
pub use runtime::background_sessions::{
    background_session_manager, BackgroundSessionAdoptOptions, BackgroundSessionListener,
    BackgroundSessionManager, BackgroundSessionStartOptions, BackgroundSessionSubscription,
};
pub use runtime::checkpoint_codec_v2::{
    checkpoint_v2_from_json, checkpoint_v2_to_json, decode_checkpoint, decode_checkpoint_bytes,
    encode_checkpoint_v1, migrate_terminal_v1, DecodedCheckpoint,
};
pub use runtime::shell::{
    build_shell_invocation, prepare_shell_execution, resolve_shell_invocation,
    PreparedShellCommand, ShellInvocation,
};
pub use runtime::state::{Checkpoint, InMemoryStateStore, StateStore};
pub use runtime::state_v2::{
    CheckpointStoreV2, CheckpointV2, EventOutboxEntry, ExtensionStateEntry, OperationError,
    OperationJournalEntry,
};
pub use runtime::stores::memory_v2::InMemoryCheckpointStoreV2;
pub use runtime::stores::redis::RedisStateStore;
pub use runtime::stores::redis_v2::RedisCheckpointStoreV2;
pub use runtime::stores::sqlite::SqliteStateStore;
pub use runtime::stores::sqlite_v2::SqliteCheckpointStoreV2;
pub use runtime::sub_task_manager::{
    ManagedSubTask, ManagedSubTaskSnapshot, SubTaskManager, SubTaskSessionAttachment,
    SubTaskTurnSnapshot,
};
pub use runtime::{
    _register_sub_agent_session, _unregister_sub_agent_session, continue_sub_agent_session,
    get_sub_agent_session, register_sub_agent_session, steer_sub_agent_session,
    sub_agent_session_registry, subscribe_sub_agent_session, unregister_sub_agent_session,
    AfterCycleAction, AfterCycleDecision, AfterCycleHook, AfterCycleSnapshot, AfterLlmEvent,
    AfterToolCallEvent, AgentRuntime, BeforeCycleMessageProvider, BeforeLlmEvent, BeforeLlmPatch,
    BeforeMemoryCompactEvent, BeforeToolCallEvent, BeforeToolCallPatch, CancellationToken,
    CancelledError, CycleRunRequest, CycleRunner, ExecutionContext, InterruptionMessageProvider,
    NativeCycleOutcome, NativeCycleOutcomeKind, RuntimeEventHandler, RuntimeHook,
    RuntimeHookManager, RuntimeRunControls, StreamCallback, SubAgentSession,
    SubAgentSessionListener, SubAgentSessionRegistry, SubAgentSessionUnsubscribe, ToolCallRunner,
    ToolRunOutcome, ToolRunRequest, MAX_PROMPT_TOO_LONG_RETRIES, MAX_PTL_RETRIES,
};
pub use sessions::{
    session_store_conformance, MemorySession, MemorySessionStore, RedisSessionStore, Session,
    SessionItem, SessionStore, SqliteSessionStore,
};
pub use tools::{
    build_default_registry, dispatch_tool_call, AgentTool, AgentToolBuilder, ApprovalDecision,
    ApprovalPolicy, ApprovalPredicate, ApprovalRequirement, BackgroundAgentTask,
    BackgroundAgentTaskBuilder, BackgroundAgentTaskHandle, BackgroundAgentTaskSnapshot,
    FunctionTool, StaticTool, Tool, ToolApprovalRule, ToolContext, ToolError, ToolExecutor,
    ToolExposure, ToolFuture, ToolHandler, ToolNotFoundError, ToolOrchestrator, ToolOutput,
    ToolPolicy, ToolRegistry, ToolRunContext, ToolRunOptions, ToolSpec, ToolSpecContext,
    ToolSpecExecutor, ToolSpecKind,
};
pub use tracing::{JsonlTraceExporter, Span, TraceSink};
pub use types::{
    AgentResult, AgentStatus, AgentTask, CacheUsage, CacheUsageStatus, CompletionReason,
    CycleRecord, CycleStatus, LLMResponse, Message, MessageRole, NoToolPolicy, SubAgentConfig,
    SubAgentConfigValidationError, SubTaskOutcome, SubTaskRequest, TaskTokenUsage, TokenUsage,
    ToolCall, ToolDirective, ToolExecutionResult, ToolResultStatus, UsageSource,
    INVALID_SUB_AGENT_MODEL_CODE, INVALID_SUB_AGENT_MODEL_MESSAGE,
    INVALID_SUB_AGENT_SYSTEM_PROMPT_CODE, INVALID_SUB_AGENT_SYSTEM_PROMPT_MESSAGE,
};
pub use workspace::{
    validate_portable_exclude_pattern, DiscoveryFilteredWorkspaceBackend, FileInfo,
    LocalWorkspaceBackend, MemoryWorkspaceBackend, PortableRegexError, S3WorkspaceBackend,
    S3WorkspaceConfig, WorkspaceBackend, INVALID_EXCLUDE_FILES_PATTERN_CODE,
    INVALID_EXCLUDE_FILES_PATTERN_MESSAGE,
};
