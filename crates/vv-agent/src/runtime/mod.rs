pub mod backends;
pub mod background_sessions;
pub mod cancellation;
pub mod context;
mod cycle_runner;
pub mod engine;
pub mod hooks;
pub mod processes;
mod results;
pub mod shell;
pub mod state;
pub mod stores;
pub mod sub_agent_sessions;
mod sub_agents;
pub mod sub_task_manager;
pub mod token_usage;
mod tool_call_runner;
mod tool_planner;

pub use background_sessions::{
    background_session_manager, BackgroundSessionListener, BackgroundSessionManager,
    BackgroundSessionSubscription,
};
pub use cancellation::CancellationToken;
pub use context::{ExecutionContext, StreamCallback};
pub use engine::{
    AgentRuntime, BeforeCycleMessageProvider, RuntimeEventHandler, RuntimeLogCallback,
    RuntimeLogHandler, RuntimeRunControls,
};
pub use hooks::{
    AfterLlmEvent, AfterToolCallEvent, BeforeLlmEvent, BeforeLlmPatch, BeforeMemoryCompactEvent,
    BeforeToolCallEvent, BeforeToolCallPatch, RuntimeHook, RuntimeHookManager,
};
pub use processes::{
    kill_process_tree, read_captured_output, remove_captured_output, start_captured_process,
    start_captured_process_with_env, wait_for_child, CapturedProcess,
};
pub use sub_agent_sessions::{
    continue_sub_agent_session, get_sub_agent_session, register_sub_agent_session,
    steer_sub_agent_session, sub_agent_session_registry, subscribe_sub_agent_session,
    unregister_sub_agent_session, SubAgentSession, SubAgentSessionListener,
    SubAgentSessionRegistry, SubAgentSessionUnsubscribe,
};
pub use sub_task_manager::SubTaskManager;
pub use token_usage::{normalize_token_usage, summarize_task_token_usage};
pub use tool_planner::patch_dynamic_tool_schema_hints;
