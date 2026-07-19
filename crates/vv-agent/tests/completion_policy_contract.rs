use std::sync::{Arc, Mutex};

use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use vv_agent::{
    Agent, AgentStatus, BeforeLlmEvent, BeforeLlmPatch, CompletionReason, FunctionTool,
    LLMResponse, LlmRequest, ModelRef, NoToolPolicy, RunConfig, Runner, RuntimeHook, ScriptStep,
    ScriptedModelProvider, ToolCall, ToolOutput, ToolUseBehavior,
};

const FIXTURE: &str = include_str!("fixtures/parity/completion_policy_v1.json");
const REASONING_HISTORY_FIXTURE: &str =
    include_str!("fixtures/parity/assistant_reasoning_history_v1.json");
const FIXTURE_SHA256: &str = "84c75dacbb43659a07c0fa1347c83e44a609a411d7896cae5a2276a3b6792135";
const CONTINUATION_HINT: &str = "Continue. If the task is complete, call task_finish.";

#[derive(Debug, Deserialize)]
struct CompletionContract {
    version: u32,
    policy_values: Vec<String>,
    framework_default: String,
    completion_reason_values: Vec<String>,
    rules: CompletionRules,
    cases: Vec<CompletionCase>,
}

#[derive(Debug, Deserialize)]
struct CompletionRules {
    assistant_text_is_not_classified: bool,
    completion_policy_does_not_change_tool_availability: bool,
    explicit_tool_directive_precedes_no_tool_policy: bool,
    partial_output_only_for_non_completed_status: bool,
    budget_exhausted_is_defined_by_run_budget_v1: bool,
    approval_resume_uses_fresh_cycle_budget: bool,
    approval_resume_preserves_resource_budget: bool,
    approved_resume_rejects_input_before_claim: bool,
    pre_cancelled_approval_resume_skips_side_effects: bool,
    guardrail_allow_preserves_completion_observation: bool,
    ordinary_llm_failure_is_typed_terminal: bool,
}

#[derive(Debug, Deserialize)]
struct CompletionCase {
    name: String,
    agent_policy: Option<String>,
    runner_default_policy: Option<String>,
    run_policy: Option<String>,
    max_cycles: u32,
    tool_use_behavior: String,
    #[serde(default)]
    stop_at_tool_names: Vec<String>,
    steps: Vec<CompletionStep>,
    expected: CompletionExpected,
}

#[derive(Debug, Deserialize)]
struct CompletionStep {
    assistant_output: String,
    #[serde(default)]
    tool_calls: Vec<FixtureToolCall>,
}

#[derive(Debug, Deserialize)]
struct FixtureToolCall {
    id: String,
    name: String,
    arguments: Value,
}

#[derive(Debug, Deserialize)]
struct CompletionExpected {
    effective_policy: String,
    status: String,
    completion_reason: String,
    completion_tool_name: Option<String>,
    final_answer: Option<String>,
    wait_reason: Option<String>,
    partial_output: Option<String>,
    cycles: usize,
    continuation_hint_emitted: bool,
}

#[derive(Default)]
struct PolicyCapture(Mutex<Vec<NoToolPolicy>>);

impl RuntimeHook for PolicyCapture {
    fn before_llm(&self, event: BeforeLlmEvent<'_>) -> Option<BeforeLlmPatch> {
        self.0
            .lock()
            .expect("policy capture")
            .push(event.task.no_tool_policy);
        None
    }
}

fn contract() -> CompletionContract {
    assert_eq!(
        format!("{:x}", Sha256::digest(FIXTURE.as_bytes())),
        FIXTURE_SHA256
    );
    serde_json::from_str(FIXTURE).expect("completion policy fixture")
}

#[test]
fn completion_policy_fixture_declares_the_public_closed_sets() {
    let contract = contract();
    assert_eq!(contract.version, 1);
    assert_eq!(contract.framework_default, "continue");
    assert_eq!(contract.policy_values, ["continue", "wait_user", "finish"]);
    assert_eq!(
        contract.completion_reason_values,
        [
            "tool_finish",
            "no_tool_finish",
            "stop_on_first_tool",
            "stop_at_tool_name",
            "wait_user",
            "max_cycles",
            "cancelled",
            "failed",
            "budget_exhausted",
        ]
    );
    assert!(contract.rules.assistant_text_is_not_classified);
    assert!(
        contract
            .rules
            .completion_policy_does_not_change_tool_availability
    );
    assert!(
        contract
            .rules
            .explicit_tool_directive_precedes_no_tool_policy
    );
    assert!(contract.rules.partial_output_only_for_non_completed_status);
    assert!(contract.rules.budget_exhausted_is_defined_by_run_budget_v1);
    assert!(contract.rules.approval_resume_uses_fresh_cycle_budget);
    assert!(contract.rules.approval_resume_preserves_resource_budget);
    assert!(contract.rules.approved_resume_rejects_input_before_claim);
    assert!(
        contract
            .rules
            .pre_cancelled_approval_resume_skips_side_effects
    );
    assert!(
        contract
            .rules
            .guardrail_allow_preserves_completion_observation
    );
    assert!(contract.rules.ordinary_llm_failure_is_typed_terminal);
    assert_eq!(NoToolPolicy::default(), NoToolPolicy::Continue);
    assert_eq!(
        CompletionReason::parse("budget_exhausted"),
        Some(CompletionReason::BudgetExhausted)
    );
}

#[tokio::test]
async fn real_runner_matches_every_canonical_completion_matrix_case() {
    for case in contract().cases {
        run_case(case).await;
    }
}

async fn run_case(case: CompletionCase) {
    let requests = Arc::new(Mutex::new(Vec::<LlmRequest>::new()));
    let steps = case
        .steps
        .iter()
        .map(|step| {
            let response = LLMResponse::with_tool_calls(
                step.assistant_output.clone(),
                step.tool_calls
                    .iter()
                    .map(|call| {
                        ToolCall::from_raw_arguments(
                            call.id.clone(),
                            call.name.clone(),
                            call.arguments.clone(),
                        )
                    })
                    .collect(),
            );
            let requests = requests.clone();
            ScriptStep::callback(move |request| {
                requests.lock().expect("requests").push(request.clone());
                Ok(response.clone())
            })
        })
        .collect();
    let provider = ScriptedModelProvider::from_steps("scripted", "completion-model", steps);
    let capture = Arc::new(PolicyCapture::default());
    let lookup = FunctionTool::builder("lookup")
        .handler(|context, _arguments: Value| async move {
            let output = match context.tool_call_id.as_str() {
                "lookup-first" => "found",
                "lookup-named" => "named result",
                other => panic!("unexpected lookup call: {other}"),
            };
            Ok(ToolOutput::text(output))
        })
        .build()
        .expect("lookup tool");
    let mut agent_builder = Agent::builder("completion-agent")
        .instructions("Follow the scripted completion case.")
        .model(ModelRef::named("completion-model"))
        .tool(lookup)
        .hook(capture.clone())
        .tool_use_behavior(match case.tool_use_behavior.as_str() {
            "run_llm_again" => ToolUseBehavior::RunLlmAgain,
            "stop_on_first_tool" => ToolUseBehavior::StopOnFirstTool,
            "stop_at_tool_names" => {
                ToolUseBehavior::StopAtToolNames(case.stop_at_tool_names.clone())
            }
            other => panic!("unsupported tool behavior: {other}"),
        });
    if let Some(policy) = case.agent_policy.as_deref() {
        agent_builder = agent_builder.no_tool_policy(parse_policy(policy));
    }
    let agent = agent_builder.build().expect("agent");
    let mut runner_builder = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace");
    if let Some(policy) = case.runner_default_policy.as_deref() {
        runner_builder = runner_builder.default_run_config(
            RunConfig::builder()
                .no_tool_policy(parse_policy(policy))
                .build(),
        );
    }
    let runner = runner_builder.build().expect("runner");
    let mut run_config = RunConfig::builder().max_cycles(case.max_cycles);
    if let Some(policy) = case.run_policy.as_deref() {
        run_config = run_config.no_tool_policy(parse_policy(policy));
    }

    let result = runner
        .run_with_config(&agent, "run completion case", run_config.build())
        .await
        .unwrap_or_else(|error| panic!("{} failed: {error}", case.name));

    assert_eq!(
        status_name(result.status()),
        case.expected.status,
        "{}",
        case.name
    );
    assert_eq!(
        result.completion_reason(),
        CompletionReason::parse(&case.expected.completion_reason),
        "{}",
        case.name
    );
    assert_eq!(
        result.completion_tool_name(),
        case.expected.completion_tool_name.as_deref(),
        "{}",
        case.name
    );
    assert_eq!(
        result.result().final_answer.as_deref(),
        case.expected.final_answer.as_deref(),
        "{}",
        case.name
    );
    assert_eq!(
        result.result().wait_reason.as_deref(),
        case.expected.wait_reason.as_deref(),
        "{}",
        case.name
    );
    assert_eq!(
        result.partial_output(),
        case.expected.partial_output.as_deref(),
        "{}",
        case.name
    );
    assert_eq!(
        result.result().cycles.len(),
        case.expected.cycles,
        "{}",
        case.name
    );
    assert_eq!(
        result
            .result()
            .messages
            .iter()
            .any(|message| message.content == CONTINUATION_HINT),
        case.expected.continuation_hint_emitted,
        "{}",
        case.name
    );
    assert_eq!(
        capture.0.lock().expect("captured policy")[0],
        parse_policy(&case.expected.effective_policy),
        "{}",
        case.name
    );
    assert!(
        requests
            .lock()
            .expect("requests")
            .iter()
            .all(|request| request.tools.iter().any(|schema| {
                schema.pointer("/function/name").and_then(Value::as_str) == Some("task_finish")
            })),
        "{} changed task_finish availability",
        case.name
    );
    if result.status() == AgentStatus::Completed {
        assert_eq!(result.partial_output(), None, "{}", case.name);
    }
    assert_ne!(
        result.completion_reason(),
        Some(CompletionReason::BudgetExhausted),
        "{}",
        case.name
    );
}

#[tokio::test]
async fn reasoning_only_continue_preserves_history_and_usage_for_next_request() {
    let contract: Value =
        serde_json::from_str(REASONING_HISTORY_FIXTURE).expect("reasoning history fixture");
    let runtime_case = &contract["runtime_case"];
    let reasoning = runtime_case["first_response"]["reasoning_content"]
        .as_str()
        .expect("reasoning content");
    let no_tool_policy = runtime_case["no_tool_policy"]
        .as_str()
        .expect("no-tool policy");
    let requests = Arc::new(Mutex::new(Vec::<LlmRequest>::new()));

    let mut first_response = LLMResponse::new("");
    first_response.raw.insert(
        "reasoning_content".to_string(),
        Value::String(reasoning.to_string()),
    );
    first_response.token_usage.reasoning_tokens = 2048;
    let first_requests = requests.clone();
    let second_requests = requests.clone();
    let steps = vec![
        ScriptStep::callback(move |request| {
            first_requests
                .lock()
                .expect("first request capture")
                .push(request.clone());
            Ok(first_response.clone())
        }),
        ScriptStep::callback(move |request| {
            second_requests
                .lock()
                .expect("second request capture")
                .push(request.clone());
            Ok(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "finish-reasoning",
                    "task_finish",
                    serde_json::json!({"message": "done"}),
                )],
            ))
        }),
    ];
    let provider = ScriptedModelProvider::from_steps("scripted", "reasoning-history-model", steps);
    let agent = Agent::builder("reasoning-history-agent")
        .instructions("Follow the scripted reasoning history fixture.")
        .model(ModelRef::named("reasoning-history-model"))
        .build()
        .expect("agent");
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("runner");

    let result = runner
        .run_with_config(
            &agent,
            "run reasoning history case",
            RunConfig::builder()
                .max_cycles(2)
                .no_tool_policy(parse_policy(no_tool_policy))
                .build(),
        )
        .await
        .expect("reasoning history run");

    let captured = requests.lock().expect("captured requests");
    assert_eq!(captured.len(), 2);
    let replayed = captured[1]
        .messages
        .iter()
        .find(|message| {
            message.role == vv_agent::MessageRole::Assistant
                && message.reasoning_content.as_deref() == Some(reasoning)
        })
        .expect("reasoning-only assistant in second request");
    assert_eq!(replayed.content, "");
    assert!(runtime_case["expected"]["next_model_request_contains_reasoning_turn"] == true);
    assert_eq!(result.result().cycles[0].token_usage.reasoning_tokens, 2048);
    assert_eq!(result.status(), AgentStatus::Completed);
}

fn parse_policy(value: &str) -> NoToolPolicy {
    match value {
        "continue" => NoToolPolicy::Continue,
        "wait_user" => NoToolPolicy::WaitUser,
        "finish" => NoToolPolicy::Finish,
        other => panic!("unknown no-tool policy: {other}"),
    }
}

fn status_name(status: AgentStatus) -> String {
    serde_json::to_value(status)
        .expect("status JSON")
        .as_str()
        .expect("status string")
        .to_string()
}
