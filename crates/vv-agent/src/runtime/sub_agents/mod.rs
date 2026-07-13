mod events;
mod invocation;
mod runner;
mod session;
mod task;
mod types;

pub(crate) use invocation::with_assigned_sub_task_identity;
pub(in crate::runtime) use types::SubTaskRunControls;

pub(in crate::runtime) const RESERVED_SUB_AGENT_METADATA_KEYS: [&str; 22] = [
    "_vv_agent_agent_name",
    "_vv_agent_allowed_tools",
    "_vv_agent_disallowed_tools",
    "_vv_agent_parent_run_id",
    "_vv_agent_parent_tool_call_id",
    "_vv_agent_run_id",
    "_vv_agent_session_id",
    "_vv_agent_tool_policy_approval",
    "_vv_agent_tool_policy_can_use_tool",
    "_vv_agent_trace_id",
    "browser_scope_key",
    "is_sub_task",
    "parent_run_id",
    "parent_task_id",
    "parent_tool_call_id",
    "run_id",
    "session_id",
    "session_memory_enabled",
    "sub_agent_name",
    "task_id",
    "trace_id",
    "workspace",
];
