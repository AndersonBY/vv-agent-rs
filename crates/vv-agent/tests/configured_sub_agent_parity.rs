use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};
use vv_agent::runtime::{
    ExecutionContext, InMemoryCheckpointStore, RuntimeRunControls, SubAgentSession,
    SubAgentSessionListener, SubAgentSessionUnsubscribe, SubTaskLineage, SubTaskManager,
    SubTaskSubmissionContext,
};
use vv_agent::tools::{build_default_registry, ToolContext, ToolSpec};
use vv_agent::types::AgentTask;
use vv_agent::{validate_portable_exclude_pattern, MemoryWorkspaceBackend};
use vv_agent::{
    AgentRuntime, AgentStatus, DiscoveryFilteredWorkspaceBackend, LLMResponse, LlmClient, LlmError,
    LlmRequest, LlmStreamCallback, LocalWorkspaceBackend, ModelError, ModelProvider, ModelRef,
    ModelSettings, ResolvedModelConfig, RunContext, RunEventPayload, ScriptStep, ScriptedLlmClient,
    SubAgentConfig, SubTaskOutcome, SubTaskRequest, TokenUsage, ToolCall, ToolDirective,
    ToolExecutionResult, ToolResultStatus, WorkspaceBackend,
};

#[path = "configured_sub_agent_parity/async_lifecycle.rs"]
mod async_lifecycle;
#[path = "configured_sub_agent_parity/child_projection.rs"]
mod child_projection;
#[path = "configured_sub_agent_parity/continuation.rs"]
mod continuation;
#[path = "configured_sub_agent_parity/event_fixture.rs"]
mod event_fixture;
#[path = "configured_sub_agent_parity/lineage.rs"]
mod lineage;
#[path = "configured_sub_agent_parity/manager.rs"]
mod manager;
#[path = "configured_sub_agent_parity/manager_contract_additions.rs"]
mod manager_contract_additions;
#[path = "configured_sub_agent_parity/manager_support.rs"]
mod manager_support;
#[path = "configured_sub_agent_parity/normalization.rs"]
mod normalization;
#[path = "configured_sub_agent_parity/policy.rs"]
mod policy;
#[path = "configured_sub_agent_parity/provider_panic.rs"]
mod provider_panic;
#[path = "configured_sub_agent_parity/request_workspace.rs"]
mod request_workspace;
#[path = "configured_sub_agent_parity/stream_events.rs"]
mod stream_events;

const CONFIGURED_SUB_AGENT_FIXTURE: &str =
    include_str!("fixtures/parity/configured_sub_agent.json");
const CONFIGURED_SUB_AGENT_EVENTS_FIXTURE: &str =
    include_str!("fixtures/parity/configured_sub_agent_events.jsonl");
const MANAGER_TOOL_ENVELOPE_FIXTURE: &str =
    include_str!("fixtures/parity/manager_tool_envelope.json");

type CapturedRuntimeEvents = Vec<(String, BTreeMap<String, Value>)>;
type SharedRuntimeEvents = Arc<Mutex<CapturedRuntimeEvents>>;

fn contract() -> Value {
    serde_json::from_str(CONFIGURED_SUB_AGENT_FIXTURE).expect("configured sub-agent parity fixture")
}

fn manager_tool_contract() -> Value {
    serde_json::from_str(MANAGER_TOOL_ENVELOPE_FIXTURE)
        .expect("manager and tool envelope parity fixture")
}

fn typed_event_parts(event: &vv_agent::RunEvent) -> (String, BTreeMap<String, Value>) {
    let value = serde_json::to_value(event).expect("typed RunEvent must serialize");
    let object = value.as_object().expect("typed RunEvent must be an object");
    let name = object
        .get("type")
        .and_then(Value::as_str)
        .expect("typed RunEvent must have a type")
        .to_string();
    let payload = object
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    (name, payload)
}

fn completed_outcome(request: vv_agent::SubTaskRequest) -> SubTaskOutcome {
    SubTaskOutcome {
        task_id: "runner-task".to_string(),
        agent_name: request.agent_name,
        status: AgentStatus::Completed,
        session_id: Some("runner-session".to_string()),
        final_answer: Some("done".to_string()),
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
