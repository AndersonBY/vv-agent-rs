use std::collections::HashSet;
use std::sync::Arc;

use serde_json::{json, Value};
use tokio::sync::broadcast;

use crate::agent::Agent;
use crate::budget::BudgetUsageSnapshot;
use crate::context::RunContext;
use crate::context_providers::ContextBundle;
use crate::events::RunEvent;
use crate::guardrails::GuardrailOutcome;
use crate::result::RunResult;
use crate::run_config::{RunConfig, INITIAL_BUDGET_USAGE_METADATA_KEY};
use crate::runtime::{BeforeLlmEvent, BeforeLlmPatch, RuntimeHook};
use crate::tools::{ApprovalPolicy, ToolPolicy};
use crate::types::AgentResult;

use super::NormalizedInput;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct HandoffRequest {
    pub(super) from_agent: String,
    pub(super) to_agent: String,
    pub(super) tool_name: String,
    pub(super) input: String,
    pub(super) tool_call_id: String,
    pub(super) cycle_index: u32,
    pub(super) metadata: crate::types::Metadata,
}

pub(super) struct SingleRunOutcome {
    pub(super) result: RunResult,
    pub(super) handoff: Option<HandoffRequest>,
}

pub(super) fn apply_input_guardrails(
    agent: &Agent,
    context: &RunContext,
    input: NormalizedInput,
) -> GuardrailOutcome<NormalizedInput> {
    let mut current = input;
    for guardrail in agent.input_guardrails() {
        current = match guardrail.check(context, &current) {
            GuardrailOutcome::Allow(input) => input,
            GuardrailOutcome::Block { message } => return GuardrailOutcome::Block { message },
            GuardrailOutcome::RequireApproval { message } => {
                return GuardrailOutcome::RequireApproval { message }
            }
        };
    }
    GuardrailOutcome::Allow(current)
}

pub(super) fn apply_output_guardrails(
    agent: &Agent,
    context: &RunContext,
    mut result: AgentResult,
) -> AgentResult {
    normalize_completion_observation(&mut result);
    let mut current = result;
    if matches!(
        current.completion_reason,
        Some(
            crate::types::CompletionReason::Cancelled
                | crate::types::CompletionReason::BudgetExhausted
        )
    ) {
        return current;
    }
    for guardrail in agent.output_guardrails() {
        let status = current.status;
        let completion_reason = current.completion_reason;
        let completion_tool_name = current.completion_tool_name.clone();
        let partial_output = current.partial_output.clone();
        let budget_usage = current.budget_usage.clone();
        let budget_exhaustion = current.budget_exhaustion.clone();
        current = match guardrail.check(context, &current) {
            GuardrailOutcome::Allow(mut output) => {
                output.status = status;
                output.completion_reason = completion_reason;
                output.completion_tool_name = completion_tool_name;
                output.partial_output = partial_output;
                output.budget_usage = budget_usage;
                output.budget_exhaustion = budget_exhaustion;
                normalize_completion_observation(&mut output);
                output
            }
            GuardrailOutcome::Block { message } | GuardrailOutcome::RequireApproval { message } => {
                let mut failed = current.clone();
                let candidate_output = crate::types::last_assistant_output(&failed.cycles)
                    .or_else(|| failed.partial_output.clone());
                failed.status = crate::types::AgentStatus::Failed;
                failed.completion_reason = Some(crate::types::CompletionReason::Failed);
                failed.completion_tool_name = None;
                failed.partial_output = candidate_output;
                failed.error = Some(message);
                failed.final_answer = None;
                failed.wait_reason = None;
                return failed;
            }
        };
    }
    current
}

fn normalize_completion_observation(result: &mut AgentResult) {
    match result.status {
        crate::types::AgentStatus::Completed => {
            result.partial_output = None;
            result.error = None;
            result.wait_reason = None;
        }
        crate::types::AgentStatus::WaitUser => {
            result.completion_reason = Some(crate::types::CompletionReason::WaitUser);
            result.partial_output = result
                .partial_output
                .clone()
                .or_else(|| crate::types::last_assistant_output(&result.cycles));
            result.final_answer = None;
            result.error = None;
        }
        crate::types::AgentStatus::MaxCycles => {
            result.completion_reason = Some(crate::types::CompletionReason::MaxCycles);
            result.completion_tool_name = None;
            result.partial_output = result
                .partial_output
                .clone()
                .or_else(|| crate::types::last_assistant_output(&result.cycles));
            result.wait_reason = None;
        }
        crate::types::AgentStatus::Failed => {
            if !matches!(
                result.completion_reason,
                Some(
                    crate::types::CompletionReason::Cancelled
                        | crate::types::CompletionReason::BudgetExhausted
                )
            ) {
                result.completion_reason = Some(crate::types::CompletionReason::Failed);
            }
            result.completion_tool_name = None;
            result.partial_output = result
                .partial_output
                .clone()
                .or_else(|| crate::types::last_assistant_output(&result.cycles));
            result.final_answer = None;
            result.wait_reason = None;
        }
        crate::types::AgentStatus::Pending | crate::types::AgentStatus::Running => {
            result.completion_reason = None;
            result.completion_tool_name = None;
            result.partial_output = None;
        }
    }
}

pub(super) fn apply_cancellation_precedence(
    mut result: AgentResult,
    cancellation_token: Option<&crate::runtime::CancellationToken>,
) -> AgentResult {
    if result.status == crate::types::AgentStatus::Failed
        && cancellation_token.is_some_and(crate::runtime::CancellationToken::is_cancelled)
    {
        result.completion_reason = Some(crate::types::CompletionReason::Cancelled);
        result.completion_tool_name = None;
        result.partial_output = result
            .partial_output
            .or_else(|| crate::types::last_assistant_output(&result.cycles));
        result.error = cancellation_token.and_then(crate::runtime::CancellationToken::reason);
        result.budget_exhaustion = None;
        result.final_answer = None;
        result.wait_reason = None;
    }
    result
}

pub(super) fn max_handoff_depth(default_config: &RunConfig, config: &RunConfig) -> u32 {
    config
        .max_handoffs
        .or(default_config.max_handoffs)
        .unwrap_or(10)
}

pub(super) fn effective_session_id(
    default_config: &RunConfig,
    config: &RunConfig,
) -> Option<String> {
    config
        .session
        .as_ref()
        .or(default_config.session.as_ref())
        .map(|session| session.session_id().trim())
        .filter(|session_id| !session_id.is_empty())
        .map(str::to_string)
        .or_else(|| {
            config
                .metadata
                .get("session_id")
                .or_else(|| default_config.metadata.get("session_id"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|session_id| !session_id.is_empty())
                .map(str::to_string)
        })
}

pub(super) fn initial_budget_usage(
    default_metadata: &crate::types::Metadata,
    run_metadata: &crate::types::Metadata,
) -> Result<Option<BudgetUsageSnapshot>, String> {
    let value = run_metadata
        .get(INITIAL_BUDGET_USAGE_METADATA_KEY)
        .or_else(|| default_metadata.get(INITIAL_BUDGET_USAGE_METADATA_KEY));
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(_)) => serde_json::from_value(value.cloned().expect("matched value"))
            .map(Some)
            .map_err(|error| {
                format!(
                    "RunConfig.metadata[{INITIAL_BUDGET_USAGE_METADATA_KEY:?}] is invalid: {error}"
                )
            }),
        Some(_) => Err(format!(
            "RunConfig.metadata[{INITIAL_BUDGET_USAGE_METADATA_KEY:?}] must be an object"
        )),
    }
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
) -> Result<(), String> {
    if let Some(store) = event_store {
        if let Err(error) = store.append(&event) {
            if event_store_fail_closed {
                return Err(format!("run event store append failed: {error}"));
            }
            eprintln!("warning: run event store append failed: {error}");
        }
    }
    if let Some(collector) = collector {
        collector
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(event.clone());
    }
    if let Some(sender) = event_sender {
        let _ = sender.send(event);
    }
    Ok(())
}

pub(super) fn insert_context_metadata(
    metadata: &mut crate::types::Metadata,
    bundle: &ContextBundle,
) {
    metadata.insert(
        "system_prompt_sections".to_string(),
        Value::Array(bundle.metadata_sections()),
    );
    metadata.insert(
        "system_prompt_sources".to_string(),
        json!(bundle.sources.clone()),
    );
    metadata.insert(
        "system_prompt_stable_hash".to_string(),
        Value::String(bundle.stable_hash.clone()),
    );
    metadata.insert(
        "system_prompt_omitted_sections".to_string(),
        json!(bundle.omitted_section_ids.clone()),
    );
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

pub(super) fn extract_handoff(result: &AgentResult) -> Option<HandoffRequest> {
    result
        .cycles
        .iter()
        .flat_map(|cycle| {
            cycle
                .tool_results
                .iter()
                .map(move |tool_result| (cycle.index, tool_result))
        })
        .find_map(|(cycle_index, tool_result)| {
            let is_handoff = tool_result
                .metadata
                .get("mode")
                .and_then(Value::as_str)
                .is_some_and(|mode| mode == "handoff");
            if !is_handoff {
                return None;
            }
            let from_agent = tool_result
                .metadata
                .get("handoff_from")
                .and_then(Value::as_str)?
                .to_string();
            let to_agent = tool_result
                .metadata
                .get("handoff_to")
                .and_then(Value::as_str)?
                .to_string();
            let tool_name = tool_result
                .metadata
                .get("handoff_tool_name")
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
                tool_name,
                input,
                tool_call_id: tool_result.tool_call_id.clone(),
                cycle_index,
                metadata: tool_result.metadata.clone(),
            })
        })
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
    let mut seen_disallowed = HashSet::new();
    merged
        .disallowed_tools
        .retain(|tool| seen_disallowed.insert(tool.clone()));
    let predicates = [
        agent.can_use_tool.clone(),
        runner.can_use_tool.clone(),
        run.can_use_tool.clone(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    merged.can_use_tool = if predicates.is_empty() {
        None
    } else {
        Some(Arc::new(move |tool_name, arguments| {
            predicates
                .iter()
                .all(|predicate| predicate(tool_name, arguments))
        }))
    };
    merged.approval = [run.approval, agent.approval, runner.approval]
        .into_iter()
        .find(|approval| !matches!(approval, ApprovalPolicy::Default))
        .unwrap_or(ApprovalPolicy::Default);
    merged
}

pub(super) struct ApprovalHook {
    policy: ToolPolicy,
}

impl ApprovalHook {
    pub(super) fn new(policy: ToolPolicy) -> Self {
        Self { policy }
    }
}

impl RuntimeHook for ApprovalHook {
    fn before_llm(&self, event: BeforeLlmEvent<'_>) -> Option<BeforeLlmPatch> {
        let filtered = event
            .tool_schemas
            .iter()
            .filter(|schema| {
                schema["function"]["name"]
                    .as_str()
                    .map(|name| tool_allowed_by_policy(&self.policy, name))
                    .unwrap_or_else(|| self.policy.allowed_tools.is_none())
            })
            .cloned()
            .collect::<Vec<_>>();
        if filtered.len() == event.tool_schemas.len() {
            return None;
        }
        Some(BeforeLlmPatch {
            messages: None,
            tool_schemas: Some(filtered),
        })
    }
}

fn tool_allowed_by_policy(policy: &ToolPolicy, tool_name: &str) -> bool {
    let allowed = policy
        .allowed_tools
        .as_ref()
        .is_none_or(|tools| tools.iter().any(|tool| tool == tool_name));
    allowed && !policy.disallowed_tools.iter().any(|tool| tool == tool_name)
}

#[cfg(test)]
mod tests {
    use super::merged_tool_policy;
    use crate::tools::{ApprovalPolicy, ToolPolicy};
    use serde_json::json;

    fn policy(approval: ApprovalPolicy) -> ToolPolicy {
        ToolPolicy {
            approval,
            ..ToolPolicy::default()
        }
    }

    #[test]
    fn approval_policy_precedence_matrix_is_run_then_agent_then_runner_then_default() {
        let cases = [
            (
                "framework default",
                ApprovalPolicy::Default,
                ApprovalPolicy::Default,
                ApprovalPolicy::Default,
                ApprovalPolicy::Default,
            ),
            (
                "runner fallback",
                ApprovalPolicy::Always,
                ApprovalPolicy::Default,
                ApprovalPolicy::Default,
                ApprovalPolicy::Always,
            ),
            (
                "agent overrides runner",
                ApprovalPolicy::Always,
                ApprovalPolicy::Never,
                ApprovalPolicy::Default,
                ApprovalPolicy::Never,
            ),
            (
                "agent on-request overrides runner",
                ApprovalPolicy::Always,
                ApprovalPolicy::OnRequest,
                ApprovalPolicy::Default,
                ApprovalPolicy::OnRequest,
            ),
            (
                "run never overrides agent",
                ApprovalPolicy::Never,
                ApprovalPolicy::Always,
                ApprovalPolicy::Never,
                ApprovalPolicy::Never,
            ),
            (
                "run always overrides agent",
                ApprovalPolicy::Always,
                ApprovalPolicy::Never,
                ApprovalPolicy::Always,
                ApprovalPolicy::Always,
            ),
            (
                "run on-request overrides agent",
                ApprovalPolicy::Never,
                ApprovalPolicy::Always,
                ApprovalPolicy::OnRequest,
                ApprovalPolicy::OnRequest,
            ),
        ];

        for (name, runner, agent, run, expected) in cases {
            let merged = merged_tool_policy(&policy(agent), &policy(runner), &policy(run));
            assert_eq!(merged.approval, expected, "{name}");
        }
    }

    #[test]
    fn merge_overrides_allowlist_deduplicates_denylist_and_ands_predicates() {
        let agent = ToolPolicy::default()
            .allow_only(["agent"])
            .disallow("shared")
            .disallow("agent")
            .can_use_tool(|_name, arguments| arguments["agent"] == json!(true));
        let runner = ToolPolicy::default()
            .allow_only(["runner"])
            .disallow("shared")
            .disallow("runner")
            .can_use_tool(|_name, arguments| arguments["runner"] == json!(true));
        let run = ToolPolicy::default()
            .allow_only(["run"])
            .disallow("agent")
            .disallow("run")
            .can_use_tool(|_name, arguments| arguments["run"] == json!(true));

        let merged = merged_tool_policy(&agent, &runner, &run);

        assert_eq!(merged.allowed_tools, Some(vec!["run".to_string()]));
        assert_eq!(
            merged.disallowed_tools,
            vec!["shared", "agent", "runner", "run"]
        );
        assert!(merged.allows_arguments(
            "tool",
            &[
                ("agent".to_string(), json!(true)),
                ("runner".to_string(), json!(true)),
                ("run".to_string(), json!(true)),
            ]
            .into_iter()
            .collect()
        ));
        assert!(!merged.allows_arguments(
            "tool",
            &[
                ("agent".to_string(), json!(true)),
                ("runner".to_string(), json!(false)),
                ("run".to_string(), json!(true)),
            ]
            .into_iter()
            .collect()
        ));
    }
}
