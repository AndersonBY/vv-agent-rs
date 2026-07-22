use std::collections::{BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use vv_agent::budget::BudgetEvaluator;
use vv_agent::{
    Agent, AgentStatus, ApprovalPolicy, BudgetDimension, BudgetEnforcementBoundary,
    BudgetExhaustion, BudgetExhaustionReason, BudgetUnavailableReason, BudgetUsageSnapshot,
    CacheUsage, CacheUsageStatus, CancellationToken, CompletionReason, FunctionTool, HostCost,
    HostCostMeter, LLMResponse, ModelRef, NoToolPolicy, RunBudgetLimits, RunConfig, Runner,
    ScriptStep, ScriptedModelProvider, TokenUsage, ToolCall, ToolOutput, ToolPolicy,
    ToolUseBehavior, UnavailableMetricPolicy, UsageSource, MAX_WIRE_INTEGER,
};

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/parity/run_budget.json"))
        .expect("run budget fixture must be valid JSON")
}

#[derive(Clone)]
struct FakeClock {
    readings: Arc<Mutex<VecDeque<u128>>>,
    last: Arc<Mutex<u128>>,
}

impl FakeClock {
    fn milliseconds(readings: impl IntoIterator<Item = u64>) -> Self {
        let readings = readings
            .into_iter()
            .map(|value| u128::from(value) * 1_000_000)
            .collect();
        Self {
            readings: Arc::new(Mutex::new(readings)),
            last: Arc::new(Mutex::new(0)),
        }
    }

    fn callback(&self) -> vv_agent::budget::MonotonicClock {
        let clock = self.clone();
        Arc::new(move || {
            let next = clock
                .readings
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pop_front();
            let mut last = clock
                .last
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(next) = next {
                *last = next;
            }
            *last
        })
    }
}

#[derive(Clone)]
struct ScriptedMeter {
    readings: Arc<Mutex<VecDeque<MeterReading>>>,
    last: Arc<Mutex<MeterReading>>,
}

type MeterReading = Result<Option<HostCost>, String>;

impl ScriptedMeter {
    fn new(readings: Vec<MeterReading>) -> Self {
        let last = readings.last().cloned().unwrap_or(Ok(None));
        Self {
            readings: Arc::new(Mutex::new(readings.into())),
            last: Arc::new(Mutex::new(last)),
        }
    }
}

impl HostCostMeter for ScriptedMeter {
    fn read(&self) -> Result<Option<HostCost>, String> {
        let next = self
            .readings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front();
        let mut last = self
            .last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(next) = next {
            *last = next;
        }
        last.clone()
    }
}

fn usage(total: u64, uncached: Option<u64>) -> TokenUsage {
    TokenUsage {
        total_tokens: Some(total),
        usage_source: UsageSource::ProviderReported,
        cache_usage: CacheUsage {
            status: if uncached.is_some() {
                CacheUsageStatus::ProviderReported
            } else {
                CacheUsageStatus::AccountingMissing
            },
            uncached_input_tokens: uncached,
            ..CacheUsage::default()
        },
        ..TokenUsage::default()
    }
}

fn evaluator(
    limits: RunBudgetLimits,
    meter: Option<Arc<dyn HostCostMeter>>,
    initial_usage: Option<BudgetUsageSnapshot>,
    milliseconds: impl IntoIterator<Item = u64>,
) -> BudgetEvaluator {
    let clock = FakeClock::milliseconds(milliseconds);
    BudgetEvaluator::with_clock(limits, meter, initial_usage, clock.callback())
        .expect("test budget must be configured")
}

#[test]
fn budget_wire_examples_round_trip() {
    let fixture = fixture();
    let wire = &fixture["wire_examples"];

    let limits: RunBudgetLimits =
        serde_json::from_value(wire["limits"].clone()).expect("limits decode");
    let snapshot: BudgetUsageSnapshot =
        serde_json::from_value(wire["snapshot"].clone()).expect("snapshot decode");
    let exhaustion: BudgetExhaustion =
        serde_json::from_value(wire["exhaustion"].clone()).expect("exhaustion decode");

    assert_eq!(serde_json::to_value(limits).unwrap(), wire["limits"]);
    assert_eq!(serde_json::to_value(snapshot).unwrap(), wire["snapshot"]);
    assert_eq!(
        serde_json::to_value(exhaustion).unwrap(),
        wire["exhaustion"]
    );
}

#[test]
fn budget_limits_reject_all_contract_invalid_cases() {
    for case in fixture()["invalid_cases"]
        .as_array()
        .expect("invalid cases")
    {
        let decoded = serde_json::from_value::<RunBudgetLimits>(case["limits"].clone());
        assert!(
            decoded.is_err(),
            "invalid case unexpectedly decoded: {case}"
        );
    }
}

#[test]
fn total_tokens_equal_limit_can_finish_but_next_cycle_is_rejected() {
    let limits = RunBudgetLimits::builder()
        .max_total_tokens(10)
        .build()
        .unwrap();
    let mut evaluator = evaluator(limits, None, None, [0, 0, 0, 0]);

    assert_eq!(evaluator.run_start(), None);
    assert_eq!(evaluator.cycle_start(), None);
    assert_eq!(evaluator.llm_complete(&usage(10, Some(10))), None);
    let exhaustion = evaluator.cycle_start().expect("next LLM is rejected");

    assert_eq!(exhaustion.dimension, BudgetDimension::TotalTokens);
    assert_eq!(exhaustion.reason, BudgetExhaustionReason::LimitReached);
    assert_eq!(
        exhaustion.enforcement_boundary,
        BudgetEnforcementBoundary::CycleStart
    );
    assert_eq!(exhaustion.overshoot, Some(0));
}

#[test]
fn total_tokens_allow_one_atomic_overshoot() {
    let limits = RunBudgetLimits::builder()
        .max_total_tokens(10)
        .build()
        .unwrap();
    let mut evaluator = evaluator(limits, None, None, [0, 0, 0, 0]);
    evaluator.run_start();
    evaluator.cycle_start();

    let exhaustion = evaluator
        .llm_complete(&usage(12, Some(12)))
        .expect("completed call overshoots");

    assert_eq!(exhaustion.reason, BudgetExhaustionReason::LimitExceeded);
    assert_eq!(exhaustion.observed, Some(12));
    assert_eq!(exhaustion.overshoot, Some(2));
    assert_eq!(evaluator.snapshot().total_tokens, Some(12));
}

#[test]
fn missing_uncached_usage_is_not_zero_and_policy_is_configurable() {
    let continuing_limits = RunBudgetLimits::builder()
        .max_uncached_input_tokens(10)
        .build()
        .unwrap();
    let strict_limits = RunBudgetLimits::builder()
        .max_uncached_input_tokens(10)
        .unavailable_metric_policy(UnavailableMetricPolicy::Stop)
        .build()
        .unwrap();
    let mut continuing = evaluator(continuing_limits, None, None, [0, 0, 0, 0]);
    let mut strict = evaluator(strict_limits, None, None, [0, 0, 0, 0]);

    continuing.run_start();
    continuing.cycle_start();
    assert_eq!(continuing.llm_complete(&usage(4, None)), None);
    assert_eq!(continuing.snapshot().uncached_input_tokens, None);
    assert_eq!(
        continuing.snapshot().unavailable_dimensions[0].reason,
        BudgetUnavailableReason::UsageMissing
    );

    strict.run_start();
    strict.cycle_start();
    let exhaustion = strict
        .llm_complete(&usage(4, None))
        .expect("strict missing metric stops");
    assert_eq!(exhaustion.reason, BudgetExhaustionReason::MetricUnavailable);
    assert_eq!(
        exhaustion.unavailable_reason,
        Some(BudgetUnavailableReason::UsageMissing)
    );
}

#[test]
fn explicit_zero_uncached_usage_remains_available() {
    let limits = RunBudgetLimits::builder()
        .max_uncached_input_tokens(1)
        .build()
        .unwrap();
    let mut evaluator = evaluator(limits, None, None, [0, 0, 0, 0]);
    evaluator.run_start();
    evaluator.cycle_start();

    assert_eq!(evaluator.llm_complete(&usage(3, Some(0))), None);
    assert_eq!(evaluator.snapshot().uncached_input_tokens, Some(0));
    assert!(evaluator.snapshot().unavailable_dimensions.is_empty());
}

#[test]
fn tool_batch_preflight_is_all_or_none_and_uses_stable_precedence() {
    let limits = RunBudgetLimits::builder()
        .max_tool_calls(1)
        .max_tool_calls_by_name([("alpha", 1)])
        .build()
        .unwrap();
    let mut evaluator = evaluator(limits, None, None, [0, 0]);
    evaluator.run_start();

    let exhaustion = evaluator
        .preflight_tools(&["alpha".to_string(), "beta".to_string()])
        .expect("whole batch is rejected");

    assert_eq!(exhaustion.dimension, BudgetDimension::ToolCalls);
    assert_eq!(exhaustion.attempted_increment, Some(2));
    assert_eq!(evaluator.snapshot().tool_calls, 0);
    assert!(evaluator.snapshot().tool_calls_by_name.is_empty());
}

#[test]
fn named_tool_budget_matches_exact_name() {
    let limits = RunBudgetLimits::builder()
        .max_tool_calls_by_name([("search", 1)])
        .build()
        .unwrap();
    let mut evaluator = evaluator(limits, None, None, [0, 0, 0]);
    evaluator.run_start();

    assert_eq!(evaluator.preflight_tools(&["search_v2".to_string()]), None);
    let exhaustion = evaluator
        .preflight_tools(&["search".to_string(), "search".to_string()])
        .expect("exact name exceeds limit");

    assert_eq!(exhaustion.dimension, BudgetDimension::ToolCallsByName);
    assert_eq!(exhaustion.tool_name.as_deref(), Some("search"));
    assert_eq!(
        evaluator.snapshot().tool_calls_by_name.get("search_v2"),
        Some(&1)
    );
}

#[test]
fn host_meter_overshoot_and_non_monotonic_reading_are_typed() {
    let limit = HostCost::new("credits", 100).unwrap();
    let limits = RunBudgetLimits::builder()
        .max_host_cost(limit.clone())
        .build()
        .unwrap();
    let meter = ScriptedMeter::new(vec![
        Ok(Some(HostCost::new("credits", 0).unwrap())),
        Ok(Some(HostCost::new("credits", 0).unwrap())),
        Ok(Some(HostCost::new("credits", 120).unwrap())),
    ]);
    let mut overshoot = evaluator(limits.clone(), Some(Arc::new(meter)), None, [0, 0, 0, 0]);
    overshoot.run_start();
    overshoot.cycle_start();

    let exhaustion = overshoot
        .llm_complete(&usage(1, Some(1)))
        .expect("host cost overshoots");
    assert_eq!(exhaustion.dimension, BudgetDimension::HostCost);
    assert_eq!(exhaustion.overshoot, Some(20));

    let meter = ScriptedMeter::new(vec![
        Ok(Some(HostCost::new("credits", 50).unwrap())),
        Ok(Some(HostCost::new("credits", 40).unwrap())),
    ]);
    let mut non_monotonic = evaluator(limits, Some(Arc::new(meter)), None, [0, 0, 0]);
    non_monotonic.run_start();
    non_monotonic.cycle_start();
    assert_eq!(non_monotonic.snapshot().host_cost, None);
    assert_eq!(
        non_monotonic.snapshot().unavailable_dimensions[0].reason,
        BudgetUnavailableReason::NonMonotonic
    );
}

#[test]
fn token_sum_wire_overflow_becomes_typed_unavailable() {
    let limits = RunBudgetLimits::builder()
        .max_total_tokens(MAX_WIRE_INTEGER)
        .unavailable_metric_policy(UnavailableMetricPolicy::Stop)
        .build()
        .unwrap();
    let initial = BudgetUsageSnapshot {
        cycles: 1,
        total_tokens: Some(MAX_WIRE_INTEGER),
        ..BudgetUsageSnapshot::default()
    };
    let mut evaluator = evaluator(limits, None, Some(initial), [0, 0]);

    let exhaustion = evaluator
        .llm_complete(&usage(1, Some(0)))
        .expect("overflow stops strict accounting");

    assert_eq!(exhaustion.reason, BudgetExhaustionReason::MetricUnavailable);
    assert_eq!(
        exhaustion.unavailable_reason,
        Some(BudgetUnavailableReason::IntegerOverflow)
    );
    assert_eq!(evaluator.snapshot().total_tokens, None);
}

#[test]
fn distributed_active_elapsed_continues_without_queue_time() {
    let limits = RunBudgetLimits::builder()
        .max_wall_time_ms(1_000)
        .build()
        .unwrap();
    let initial = BudgetUsageSnapshot {
        elapsed_ms: 120,
        ..BudgetUsageSnapshot::default()
    };
    let mut evaluator = evaluator(limits, None, Some(initial), [5_000, 5_030]);

    assert_eq!(evaluator.run_start(), None);
    assert_eq!(evaluator.snapshot().elapsed_ms, 150);
}

#[test]
fn fixture_path_is_vendored_for_offline_producer_tests() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/parity/run_budget.json");
    assert!(path.is_file());
}

#[tokio::test]
async fn public_runner_matches_every_canonical_budget_case() {
    for case in fixture()["runner_cases"].as_array().expect("runner cases") {
        run_public_runner_case(case).await;
    }
}

#[tokio::test]
async fn approval_resume_preserves_reserved_tool_count_and_source_usage() {
    let executions = Arc::new(AtomicUsize::new(0));
    let tool_executions = executions.clone();
    let tool = FunctionTool::builder("guarded_delete")
        .json_schema(serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"}
            },
            "required": ["path"]
        }))
        .needs_approval(true)
        .handler(move |_context, _arguments: Value| {
            let executions = tool_executions.clone();
            async move {
                executions.fetch_add(1, Ordering::SeqCst);
                Ok(ToolOutput::text("deleted"))
            }
        })
        .build()
        .expect("guarded tool");
    let mut response = LLMResponse::with_tool_calls(
        "delete after approval",
        vec![ToolCall::from_raw_arguments(
            "delete-call",
            "guarded_delete",
            serde_json::json!({"path": "x.txt"}),
        )],
    );
    response.token_usage = usage(8, Some(8));
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "budget-model",
            vec![response],
        ))
        .workspace(tempfile::tempdir().expect("workspace").path())
        .build()
        .expect("runner");
    let agent = Agent::builder("budgeted-approver")
        .instructions("Delete only after approval.")
        .model(ModelRef::named("budget-model"))
        .tool(tool)
        .tool_policy(ToolPolicy {
            approval: ApprovalPolicy::OnRequest,
            ..ToolPolicy::default()
        })
        .tool_use_behavior(ToolUseBehavior::StopOnFirstTool)
        .build()
        .expect("agent");
    let limits = RunBudgetLimits::builder()
        .max_total_tokens(20)
        .max_tool_calls(1)
        .build()
        .unwrap();

    let interrupted = runner
        .run_with_config(
            &agent,
            "delete x.txt",
            RunConfig::builder().budget_limits(limits).build(),
        )
        .await
        .expect("interrupted run");
    let source_usage = interrupted
        .budget_usage()
        .cloned()
        .expect("source budget usage");
    assert_eq!(interrupted.status(), AgentStatus::WaitUser);
    assert_eq!(source_usage.cycles, 1);
    assert_eq!(source_usage.total_tokens, Some(8));
    assert_eq!(source_usage.tool_calls, 1);
    let interruption_id = interrupted.approvals()[0].interruption_id.clone();
    let mut state = interrupted.into_state().expect("approval state");
    state.approve(&interruption_id).expect("approve");

    let resumed = runner.resume(state).await.expect("resumed run");

    let resumed_usage = resumed.budget_usage().expect("resumed budget usage");
    assert_eq!(resumed.status(), AgentStatus::Completed);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert_eq!(resumed_usage.cycles, 1);
    assert_eq!(resumed_usage.total_tokens, Some(8));
    assert_eq!(resumed_usage.tool_calls, 1);
    assert!(resumed_usage.elapsed_ms >= source_usage.elapsed_ms);
    assert_eq!(resumed.budget_exhaustion(), None);
    assert!(resumed.events().iter().any(|event| matches!(
        event.payload(),
        vv_agent::RunEventPayload::BudgetSnapshot {
            enforcement_boundary: BudgetEnforcementBoundary::ToolBatchComplete,
            ..
        }
    )));
}

#[tokio::test]
async fn approval_continue_resumes_budget_counters_in_the_fresh_model_loop() {
    let executions = Arc::new(AtomicUsize::new(0));
    let tool_executions = executions.clone();
    let tool = FunctionTool::builder("guarded_lookup")
        .json_schema(serde_json::json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"}
            },
            "required": ["query"]
        }))
        .needs_approval(true)
        .handler(move |_context, _arguments: Value| {
            let executions = tool_executions.clone();
            async move {
                executions.fetch_add(1, Ordering::SeqCst);
                Ok(ToolOutput::text("approved result"))
            }
        })
        .build()
        .expect("guarded tool");
    let mut first = LLMResponse::with_tool_calls(
        "lookup after approval",
        vec![ToolCall::from_raw_arguments(
            "lookup-call",
            "guarded_lookup",
            serde_json::json!({"query": "item"}),
        )],
    );
    first.token_usage = usage(8, Some(8));
    let mut second = LLMResponse::new("finished after approval");
    second.token_usage = usage(4, Some(4));
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "budget-model",
            vec![first, second],
        ))
        .workspace(tempfile::tempdir().expect("workspace").path())
        .build()
        .expect("runner");
    let agent = Agent::builder("budgeted-continue")
        .instructions("Continue after approval.")
        .model(ModelRef::named("budget-model"))
        .tool(tool)
        .tool_policy(ToolPolicy {
            approval: ApprovalPolicy::OnRequest,
            ..ToolPolicy::default()
        })
        .no_tool_policy(NoToolPolicy::Finish)
        .build()
        .expect("agent");
    let limits = RunBudgetLimits::builder()
        .max_total_tokens(20)
        .max_tool_calls(1)
        .build()
        .unwrap();

    let interrupted = runner
        .run_with_config(
            &agent,
            "lookup item",
            RunConfig::builder().budget_limits(limits).build(),
        )
        .await
        .expect("interrupted run");
    let interruption_id = interrupted.approvals()[0].interruption_id.clone();
    let mut state = interrupted.into_state().expect("approval state");
    state.approve(&interruption_id).expect("approve");

    let resumed = runner.resume(state).await.expect("resumed run");

    let resumed_usage = resumed.budget_usage().expect("resumed budget usage");
    assert_eq!(resumed.status(), AgentStatus::Completed);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert_eq!(resumed_usage.cycles, 2);
    assert_eq!(resumed_usage.total_tokens, Some(12));
    assert_eq!(resumed_usage.tool_calls, 1);
    assert_eq!(resumed.budget_exhaustion(), None);
}

#[tokio::test]
async fn completed_llm_cancellation_wins_without_losing_budget_usage() {
    let cancellation = CancellationToken::default();
    let cancellation_after_llm = cancellation.clone();
    let mut response = LLMResponse::new("completed draft");
    response.token_usage = usage(12, Some(12));
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "budget-model",
        vec![ScriptStep::callback(move |_request| {
            cancellation_after_llm.cancel_with_reason("cancelled after completed LLM call");
            Ok(response.clone())
        })],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(tempfile::tempdir().expect("workspace").path())
        .build()
        .expect("runner");
    let agent = Agent::builder("cancel-after-llm")
        .instructions("Return the scripted response.")
        .model(ModelRef::named("budget-model"))
        .no_tool_policy(NoToolPolicy::Finish)
        .build()
        .expect("agent");

    let result = runner
        .run_with_config(
            &agent,
            "run",
            RunConfig::builder()
                .budget_limits(
                    RunBudgetLimits::builder()
                        .max_total_tokens(10)
                        .build()
                        .unwrap(),
                )
                .cancellation_token(cancellation)
                .build(),
        )
        .await
        .expect("cancelled run");

    assert_eq!(result.status(), AgentStatus::Failed);
    assert_eq!(
        result.completion_reason(),
        Some(CompletionReason::Cancelled)
    );
    assert_eq!(result.partial_output(), Some("completed draft"));
    let budget_usage = result.budget_usage().expect("budget usage");
    assert_eq!(budget_usage.cycles, 1);
    assert_eq!(budget_usage.total_tokens, Some(12));
    assert_eq!(result.budget_exhaustion(), None);
    assert!(!result.events().iter().any(|event| matches!(
        event.payload(),
        vv_agent::RunEventPayload::BudgetExhausted { .. }
    )));
    assert!(matches!(
        result.events().last().map(vv_agent::RunEvent::payload),
        Some(vv_agent::RunEventPayload::RunCancelled { .. })
    ));
}

#[tokio::test]
async fn completed_tool_cancellation_wins_without_losing_budget_usage() {
    let cancellation = CancellationToken::default();
    let cancellation_after_tool = cancellation.clone();
    let executions = Arc::new(AtomicUsize::new(0));
    let tool_executions = executions.clone();
    let tool = FunctionTool::builder("do_work")
        .handler(move |_context, _arguments: Value| {
            let executions = tool_executions.clone();
            async move {
                executions.fetch_add(1, Ordering::SeqCst);
                Ok(ToolOutput::text("tool side effect completed"))
            }
        })
        .build()
        .expect("tool");
    let mut response = LLMResponse::with_tool_calls(
        "tool draft",
        vec![ToolCall::from_raw_arguments(
            "work-call",
            "do_work",
            serde_json::json!({}),
        )],
    );
    response.token_usage = usage(2, Some(2));
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "budget-model",
            vec![response],
        ))
        .workspace(tempfile::tempdir().expect("workspace").path())
        .build()
        .expect("runner");
    let agent = Agent::builder("cancel-after-tool")
        .instructions("Call the scripted tool.")
        .model(ModelRef::named("budget-model"))
        .tool(tool)
        .build()
        .expect("agent");
    let meter = ScriptedMeter::new(vec![
        Ok(Some(HostCost::new("credits", 0).unwrap())),
        Ok(Some(HostCost::new("credits", 0).unwrap())),
        Ok(Some(HostCost::new("credits", 0).unwrap())),
        Ok(Some(HostCost::new("credits", 0).unwrap())),
        Ok(Some(HostCost::new("credits", 120).unwrap())),
    ]);

    let result = runner
        .run_with_config(
            &agent,
            "run",
            RunConfig::builder()
                .budget_limits(
                    RunBudgetLimits::builder()
                        .max_tool_calls(1)
                        .max_host_cost(HostCost::new("credits", 100).unwrap())
                        .build()
                        .unwrap(),
                )
                .host_cost_meter(meter)
                .cancellation_token(cancellation)
                .stream(move |event| {
                    if matches!(
                        event.payload(),
                        vv_agent::RunEventPayload::ToolCallCompleted { .. }
                    ) {
                        cancellation_after_tool
                            .cancel_with_reason("cancelled after completed tool call");
                    }
                })
                .build(),
        )
        .await
        .expect("cancelled run");

    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert_eq!(result.status(), AgentStatus::Failed);
    assert_eq!(
        result.completion_reason(),
        Some(CompletionReason::Cancelled)
    );
    assert_eq!(result.partial_output(), Some("tool draft"));
    let budget_usage = result.budget_usage().expect("budget usage");
    assert_eq!(budget_usage.tool_calls, 1);
    assert_eq!(
        budget_usage.host_cost,
        Some(HostCost::new("credits", 120).unwrap())
    );
    assert_eq!(result.budget_exhaustion(), None);
    assert!(!result.events().iter().any(|event| matches!(
        event.payload(),
        vv_agent::RunEventPayload::BudgetExhausted { .. }
    )));
    assert!(matches!(
        result.events().last().map(vv_agent::RunEvent::payload),
        Some(vv_agent::RunEventPayload::RunCancelled { .. })
    ));
}

async fn run_public_runner_case(case: &Value) {
    let name = case["name"].as_str().expect("case name");
    let model_calls = Arc::new(AtomicUsize::new(0));
    let steps = case["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .map(|step| {
            let response = scripted_response(step);
            let model_calls = model_calls.clone();
            ScriptStep::callback(move |_request| {
                model_calls.fetch_add(1, Ordering::SeqCst);
                Ok(response.clone())
            })
        })
        .collect::<Vec<_>>();
    let provider = ScriptedModelProvider::from_steps("scripted", "budget-model", steps);
    let tool_execution_count = Arc::new(AtomicUsize::new(0));
    let tool_names = case["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .flat_map(|step| {
            step["tool_calls"]
                .as_array()
                .expect("tool calls")
                .iter()
                .map(|call| call["name"].as_str().expect("tool name").to_string())
        })
        .collect::<BTreeSet<_>>();
    let mut agent_builder = Agent::builder("run-budget-contract")
        .instructions("Execute the deterministic budget fixture.")
        .model(ModelRef::named("budget-model"))
        .no_tool_policy(parse_no_tool_policy(
            case["no_tool_policy"].as_str().expect("no-tool policy"),
        ));
    for tool_name in tool_names {
        let count = tool_execution_count.clone();
        let tool = FunctionTool::builder(tool_name)
            .handler(move |_context, _arguments: Value| {
                let count = count.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok(ToolOutput::text("fixture tool result"))
                }
            })
            .build()
            .expect("fixture tool");
        agent_builder = agent_builder.tool(tool);
    }
    let agent = agent_builder.build().expect("fixture agent");
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(tempfile::tempdir().expect("workspace").path())
        .build()
        .expect("fixture runner");
    let cancellation = CancellationToken::default();
    if case["pre_cancelled"].as_bool().expect("pre-cancelled") {
        cancellation.cancel_with_reason("cancelled by fixture");
    }
    let mut config = RunConfig::builder()
        .max_cycles(
            u32::try_from(case["steps"].as_array().expect("steps").len() + 1)
                .expect("fixture cycle count")
                .max(2),
        )
        .cancellation_token(cancellation);
    if !case["limits"].is_null() {
        let limits = serde_json::from_value(case["limits"].clone()).expect("case limits");
        config = config.budget_limits(limits);
    }
    let readings = case["host_cost_readings"]
        .as_array()
        .expect("host readings")
        .iter()
        .map(|reading| {
            serde_json::from_value::<HostCost>(reading.clone())
                .map(Some)
                .map_err(|error| error.to_string())
        })
        .collect::<Vec<_>>();
    if !readings.is_empty() {
        config = config.host_cost_meter(ScriptedMeter::new(readings));
    }

    let result = runner
        .run_with_config(&agent, "run the fixture", config.build())
        .await
        .unwrap_or_else(|error| panic!("{name} failed: {error}"));
    let expected = &case["expected"];

    assert_eq!(
        status_name(result.status()),
        expected["status"].as_str().unwrap(),
        "{name}"
    );
    assert_eq!(
        result.completion_reason(),
        CompletionReason::parse(expected["completion_reason"].as_str().unwrap()),
        "{name}"
    );
    assert_eq!(
        model_calls.load(Ordering::SeqCst),
        expected["model_calls"].as_u64().unwrap() as usize,
        "{name}"
    );
    assert_eq!(
        tool_execution_count.load(Ordering::SeqCst),
        expected["tool_execution_count"].as_u64().unwrap() as usize,
        "{name}"
    );
    if let Some(partial_output) = expected.get("partial_output") {
        assert_eq!(result.partial_output(), partial_output.as_str(), "{name}");
    }
    if let Some(error) = expected.get("error") {
        assert_eq!(result.result().error.as_deref(), error.as_str(), "{name}");
    }
    if case["limits"].is_null() {
        assert_eq!(result.budget_usage(), None, "{name}");
    }
    if let Some(usage) = expected.get("usage").and_then(Value::as_object) {
        let actual = serde_json::to_value(result.budget_usage().expect("budget usage")).unwrap();
        for (key, expected_value) in usage {
            assert_eq!(&actual[key], expected_value, "{name}: usage.{key}");
        }
    }
    if let Some(expected_value) = expected.get("uncached_input_tokens") {
        assert_eq!(
            serde_json::to_value(result.budget_usage().unwrap().uncached_input_tokens).unwrap(),
            *expected_value,
            "{name}"
        );
    }
    if let Some(expected_value) = expected.get("tool_calls") {
        assert_eq!(
            result.budget_usage().unwrap().tool_calls,
            expected_value.as_u64().unwrap(),
            "{name}"
        );
    }
    if let Some(expected_value) = expected.get("unavailable_dimensions") {
        assert_eq!(
            serde_json::to_value(&result.budget_usage().unwrap().unavailable_dimensions).unwrap(),
            *expected_value,
            "{name}"
        );
    }
    assert_eq!(
        result
            .budget_exhaustion()
            .map(|value| serde_json::to_value(value).unwrap())
            .unwrap_or(Value::Null),
        expected["budget_exhaustion"],
        "{name}"
    );
    if !expected["budget_exhaustion"].is_null() {
        let tail = result
            .events()
            .iter()
            .rev()
            .take(2)
            .map(event_type)
            .collect::<Vec<_>>();
        assert_eq!(tail, ["run_failed", "budget_exhausted"], "{name}");
    }
    if let Some(expected_types) = expected.get("budget_event_types") {
        let actual = result
            .events()
            .iter()
            .map(event_type)
            .filter(|event| event.starts_with("budget_"))
            .map(str::to_string)
            .collect::<Vec<_>>();
        assert_eq!(
            Value::Array(actual.into_iter().map(Value::String).collect()),
            *expected_types,
            "{name}"
        );
    }
}

fn scripted_response(step: &Value) -> LLMResponse {
    let tool_calls = step["tool_calls"]
        .as_array()
        .expect("tool calls")
        .iter()
        .map(|call| {
            ToolCall::from_raw_arguments(
                call["id"].as_str().expect("call id"),
                call["name"].as_str().expect("call name"),
                call["arguments"].clone(),
            )
        })
        .collect();
    let mut response = LLMResponse::with_tool_calls(
        step["assistant_output"].as_str().expect("assistant output"),
        tool_calls,
    );
    response.token_usage = match step.get("usage") {
        None | Some(Value::Null) => TokenUsage::default(),
        Some(usage_value) => usage(
            usage_value["total_tokens"].as_u64().expect("total tokens"),
            usage_value["uncached_input_tokens"].as_u64(),
        ),
    };
    response
}

fn parse_no_tool_policy(value: &str) -> NoToolPolicy {
    match value {
        "continue" => NoToolPolicy::Continue,
        "wait_user" => NoToolPolicy::WaitUser,
        "finish" => NoToolPolicy::Finish,
        other => panic!("unsupported no-tool policy: {other}"),
    }
}

fn status_name(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Pending => "pending",
        AgentStatus::Running => "running",
        AgentStatus::WaitUser => "wait_user",
        AgentStatus::Completed => "completed",
        AgentStatus::Failed => "failed",
        AgentStatus::MaxCycles => "max_cycles",
        AgentStatus::ReconciliationRequired => "reconciliation_required",
    }
}

fn event_type(event: &vv_agent::RunEvent) -> &str {
    match event.payload() {
        vv_agent::RunEventPayload::BudgetSnapshot { .. } => "budget_snapshot",
        vv_agent::RunEventPayload::BudgetExhausted { .. } => "budget_exhausted",
        vv_agent::RunEventPayload::RunFailed { .. } => "run_failed",
        vv_agent::RunEventPayload::RunCompleted { .. } => "run_completed",
        vv_agent::RunEventPayload::RunCancelled { .. } => "run_cancelled",
        _ => "other",
    }
}
