use std::sync::Arc;

use serde_json::{json, Value};
use tokio::sync::broadcast;

use crate::agent::Agent;
use crate::context::RunContext;
use crate::context_providers::ContextBundle;
use crate::events::RunEvent;
use crate::guardrails::GuardrailOutcome;
use crate::result::RunResult;
use crate::run_config::RunConfig;
use crate::runtime::{BeforeToolCallEvent, BeforeToolCallPatch, RuntimeHook};
use crate::tools::{ApprovalPolicy, ToolPolicy};
use crate::types::{AgentResult, ToolDirective, ToolExecutionResult, ToolResultStatus};

use super::NormalizedInput;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct HandoffRequest {
    pub(super) from_agent: String,
    pub(super) to_agent: String,
    pub(super) input: String,
    pub(super) tool_call_id: String,
}

pub(super) struct SingleRunOutcome {
    pub(super) result: RunResult,
    pub(super) handoff: Option<HandoffRequest>,
}

pub(super) struct ApprovedToolCall {
    pub(super) call: crate::types::ToolCall,
    pub(super) cycle_index: u32,
}

pub(super) fn apply_input_guardrails(
    agent: &Agent,
    context: &RunContext,
    input: NormalizedInput,
) -> Result<NormalizedInput, String> {
    let mut current = input;
    for guardrail in agent.input_guardrails() {
        current = match guardrail.check(context, &current) {
            GuardrailOutcome::Allow(input) => input,
            GuardrailOutcome::Block { message } => return Err(message),
            GuardrailOutcome::RequireApproval { message } => return Err(message),
        };
    }
    Ok(current)
}

pub(super) fn apply_output_guardrails(
    agent: &Agent,
    context: &RunContext,
    result: AgentResult,
) -> AgentResult {
    let mut current = result;
    for guardrail in agent.output_guardrails() {
        current = match guardrail.check(context, &current) {
            GuardrailOutcome::Allow(output) => output,
            GuardrailOutcome::Block { message } | GuardrailOutcome::RequireApproval { message } => {
                let mut failed = current.clone();
                failed.status = crate::types::AgentStatus::Failed;
                failed.error = Some(message);
                failed.final_answer = None;
                failed
            }
        };
    }
    current
}

pub(super) fn max_handoff_depth(config: &RunConfig, agent: &Agent) -> u32 {
    config
        .max_cycles
        .or(agent.max_cycles())
        .unwrap_or(10)
        .max(1)
}

pub(super) fn effective_event_store(
    default_config: &RunConfig,
    config: &RunConfig,
) -> (Option<Arc<dyn crate::event_store::RunEventStore>>, bool) {
    (
        config
            .event_store
            .clone()
            .or_else(|| default_config.event_store.clone()),
        config.event_store_fail_closed || default_config.event_store_fail_closed,
    )
}

pub(super) fn capture_event(
    collector: Option<&Arc<std::sync::Mutex<Vec<RunEvent>>>>,
    event_sender: Option<&broadcast::Sender<RunEvent>>,
    event_store: Option<&Arc<dyn crate::event_store::RunEventStore>>,
    event_store_fail_closed: bool,
    event: RunEvent,
) {
    if let Some(store) = event_store {
        if let Err(error) = store.append(&event) {
            if event_store_fail_closed {
                panic!("run event store append failed: {error}");
            }
            eprintln!("warning: run event store append failed: {error}");
        }
    }
    if let Some(sender) = event_sender {
        let _ = sender.send(event.clone());
    }
    if let Some(collector) = collector {
        if let Ok(mut events) = collector.lock() {
            events.push(event);
        }
    }
}

pub(super) fn insert_context_metadata(
    metadata: &mut crate::types::Metadata,
    bundle: &ContextBundle,
) {
    metadata.insert(
        "context_section_ids".to_string(),
        json!(bundle
            .sections
            .iter()
            .map(|section| section.id.clone())
            .collect::<Vec<_>>()),
    );
    metadata.insert("context_sources".to_string(), json!(bundle.sources.clone()));
    metadata.insert(
        "context_stable_hash".to_string(),
        Value::String(bundle.stable_hash.clone()),
    );
    metadata.insert(
        "context_omitted_section_ids".to_string(),
        json!(bundle.omitted_section_ids.clone()),
    );
}

pub(super) fn find_approved_tool_call(
    result: &AgentResult,
    approved_ids: &[String],
) -> Option<ApprovedToolCall> {
    for cycle in &result.cycles {
        for tool_result in &cycle.tool_results {
            let interruption_id = tool_result
                .metadata
                .get("approval_interruption_id")
                .and_then(Value::as_str)?;
            if !approved_ids.iter().any(|id| id == interruption_id) {
                continue;
            }
            let tool_name = tool_result
                .metadata
                .get("tool_name")
                .and_then(Value::as_str)?;
            let call = cycle
                .tool_calls
                .iter()
                .find(|call| call.id == tool_result.tool_call_id && call.name == tool_name)
                .cloned()
                .or_else(|| {
                    cycle
                        .tool_calls
                        .iter()
                        .find(|call| call.name == tool_name)
                        .cloned()
                })?;
            return Some(ApprovedToolCall {
                call,
                cycle_index: cycle.index,
            });
        }
    }
    None
}

pub(super) fn extract_handoff(result: &AgentResult) -> Option<HandoffRequest> {
    result
        .cycles
        .iter()
        .flat_map(|cycle| cycle.tool_results.iter())
        .find_map(|tool_result| {
            let is_handoff = tool_result
                .metadata
                .get("handoff")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !is_handoff {
                return None;
            }
            let from_agent = tool_result
                .metadata
                .get("from_agent")
                .and_then(Value::as_str)?
                .to_string();
            let to_agent = tool_result
                .metadata
                .get("to_agent")
                .and_then(Value::as_str)?
                .to_string();
            let input = tool_result
                .metadata
                .get("handoff_input")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            Some(HandoffRequest {
                from_agent,
                to_agent,
                input,
                tool_call_id: tool_result.tool_call_id.clone(),
            })
        })
}

pub(super) fn approval_required(policy: &ToolPolicy) -> bool {
    !matches!(policy.approval, ApprovalPolicy::Never)
}

pub(super) fn merged_tool_policy(
    agent: &ToolPolicy,
    runner: &ToolPolicy,
    run: &ToolPolicy,
) -> ToolPolicy {
    let mut merged = agent.clone();
    if runner.allowed_tools.is_some() {
        merged.allowed_tools = runner.allowed_tools.clone();
    }
    if run.allowed_tools.is_some() {
        merged.allowed_tools = run.allowed_tools.clone();
    }
    merged
        .disallowed_tools
        .extend(runner.disallowed_tools.clone());
    merged.disallowed_tools.extend(run.disallowed_tools.clone());
    merged.approval = match run.approval {
        ApprovalPolicy::Never if !matches!(runner.approval, ApprovalPolicy::Never) => {
            runner.approval.clone()
        }
        ApprovalPolicy::Never if !matches!(agent.approval, ApprovalPolicy::Never) => {
            agent.approval.clone()
        }
        _ => run.approval.clone(),
    };
    if let Some(max_concurrency) = runner.max_concurrency {
        merged.max_concurrency = Some(max_concurrency);
    }
    if let Some(max_concurrency) = run.max_concurrency {
        merged.max_concurrency = Some(max_concurrency);
    }
    merged
}

pub(super) struct ApprovalHook {
    policy: ToolPolicy,
    approved_ids: Vec<String>,
}

impl ApprovalHook {
    pub(super) fn new(policy: ToolPolicy, metadata: crate::types::Metadata) -> Self {
        let approved_ids = metadata
            .get("approved_tool_interruption_ids")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            policy,
            approved_ids,
        }
    }
}

impl RuntimeHook for ApprovalHook {
    fn before_tool_call(&self, event: BeforeToolCallEvent<'_>) -> Option<BeforeToolCallPatch> {
        if let Some(allowed) = self.policy.allowed_tools.as_ref() {
            if !allowed.iter().any(|tool| tool == &event.call.name) {
                return Some(BeforeToolCallPatch::result(approval_error(
                    &event.call.id,
                    &event.call.name,
                    "tool_not_allowed",
                    "Tool is not in the allowed tool list.",
                )));
            }
        }
        if self
            .policy
            .disallowed_tools
            .iter()
            .any(|tool| tool == &event.call.name)
        {
            return Some(BeforeToolCallPatch::result(approval_error(
                &event.call.id,
                &event.call.name,
                "tool_disallowed",
                "Tool is disallowed by policy.",
            )));
        }
        if !matches!(self.policy.approval, ApprovalPolicy::Always) {
            return None;
        }
        let interruption_id = approval_interruption_id(event.task.task_id.as_str(), event.call);
        if self
            .approved_ids
            .iter()
            .any(|approved| approved == &interruption_id)
        {
            return None;
        }
        Some(BeforeToolCallPatch::result(approval_required_result(
            &event.call.id,
            &event.call.name,
            &interruption_id,
        )))
    }
}

fn approval_interruption_id(task_id: &str, call: &crate::types::ToolCall) -> String {
    format!("approval:{task_id}:{}:{}", call.name, call.id)
}

fn approval_required_result(
    tool_call_id: &str,
    tool_name: &str,
    interruption_id: &str,
) -> ToolExecutionResult {
    let mut metadata = crate::types::Metadata::new();
    metadata.insert("approval_required".to_string(), Value::Bool(true));
    metadata.insert(
        "approval_interruption_id".to_string(),
        Value::String(interruption_id.to_string()),
    );
    metadata.insert(
        "tool_name".to_string(),
        Value::String(tool_name.to_string()),
    );
    ToolExecutionResult {
        tool_call_id: tool_call_id.to_string(),
        content: json!({
            "ok": false,
            "approval_required": true,
            "interruption_id": interruption_id,
            "tool_name": tool_name,
        })
        .to_string(),
        status: ToolResultStatus::WaitResponse,
        directive: ToolDirective::WaitUser,
        error_code: Some("tool_approval_required".to_string()),
        metadata,
        image_url: None,
        image_path: None,
    }
}

fn approval_error(
    tool_call_id: &str,
    tool_name: &str,
    error_code: &str,
    message: &str,
) -> ToolExecutionResult {
    ToolExecutionResult {
        tool_call_id: tool_call_id.to_string(),
        content: json!({
            "ok": false,
            "error": message,
            "error_code": error_code,
            "tool_name": tool_name,
        })
        .to_string(),
        status: ToolResultStatus::Error,
        directive: ToolDirective::Continue,
        error_code: Some(error_code.to_string()),
        metadata: crate::types::Metadata::new(),
        image_url: None,
        image_path: None,
    }
}

pub(super) fn completed_from_first_tool_result(result: RunResult) -> RunResult {
    if result.status() != crate::types::AgentStatus::MaxCycles {
        return result;
    }
    let Some(tool_result) = result
        .result()
        .cycles
        .iter()
        .flat_map(|cycle| cycle.tool_results.iter())
        .find(|tool_result| tool_result.status == ToolResultStatus::Success)
        .cloned()
    else {
        return result;
    };
    let final_answer = tool_result.content.clone();
    let agent_name = result.agent_name().to_string();
    let resolved = result.resolved_model().clone();
    let mut agent_result = result.result().clone();
    agent_result.status = crate::types::AgentStatus::Completed;
    agent_result.final_answer = Some(final_answer);
    RunResult::new(agent_name, agent_result, resolved)
}
