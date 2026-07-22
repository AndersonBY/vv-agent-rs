use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Condvar, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use vv_agent::runtime::{
    ExecutionContext, RuntimeRunControls, SubTaskLineage, SubTaskManager, SubTaskTurnSnapshot,
};
use vv_agent::tools::{build_default_registry, ToolSpec};
use vv_agent::types::AgentTask;
use vv_agent::{
    AgentRuntime, AgentStatus, ApprovalPolicy, LLMResponse, LlmClient, LlmError, LlmRequest,
    LlmStreamCallback, MemoryWorkspaceBackend, MessageRole, RunContext, ScriptStep,
    ScriptedLlmClient, SubAgentConfig, SubAgentSession, SubTaskOutcome, SubTaskSessionAttachment,
    ToolCall, ToolExecutionResult, ToolPolicy,
};

#[path = "continuation/admission.rs"]
mod admission;
#[path = "continuation/cancellation.rs"]
mod cancellation;
#[path = "continuation/errors.rs"]
mod errors;
#[path = "continuation/lifecycle.rs"]
mod lifecycle;
#[path = "continuation/parent_turn.rs"]
mod parent_turn;
#[path = "continuation/trace.rs"]
mod trace;

const CONFIGURED_SUB_AGENT_FIXTURE: &str =
    include_str!("../fixtures/parity/configured_sub_agent.json");

fn contract() -> Value {
    serde_json::from_str(CONFIGURED_SUB_AGENT_FIXTURE).expect("configured sub-agent fixture")
}

fn typed_event_parts(event: &vv_agent::RunEvent) -> (String, BTreeMap<String, Value>) {
    super::typed_event_parts(event)
}

fn finish_response(tool_call_id: &str, message: &str) -> LLMResponse {
    LLMResponse::with_tool_calls(
        "",
        vec![ToolCall::new(
            tool_call_id,
            "task_finish",
            BTreeMap::from([("message".to_string(), json!(message))]),
        )],
    )
}

fn completed_outcome_for_manager(task_id: &str, session_id: &str) -> SubTaskOutcome {
    SubTaskOutcome {
        task_id: task_id.to_string(),
        agent_name: "researcher".to_string(),
        status: AgentStatus::Completed,
        session_id: Some(session_id.to_string()),
        final_answer: Some("initially completed".to_string()),
        wait_reason: None,
        error: None,
        error_code: None,
        completion_reason: None,
        completion_tool_name: None,
        partial_output: None,
        cycles: 1,
        todo_list: Vec::new(),
        resolved: BTreeMap::new(),
    }
}
