use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use vv_agent::{
    Agent, AgentStatus, BudgetExhaustionReason, CacheUsage, CacheUsageStatus, HostCost,
    HostCostMeter, LLMResponse, ModelRef, NoToolPolicy, RunBudgetLimits, RunConfig, Runner,
    ScriptStep, ScriptedModelProvider, TokenUsage, UsageSource,
};

#[derive(Clone)]
struct CostMeter(Arc<AtomicU64>);
impl HostCostMeter for CostMeter {
    fn read(&self) -> Result<Option<HostCost>, String> {
        Ok(Some(
            HostCost::new("credits", self.0.load(Ordering::SeqCst)).unwrap(),
        ))
    }
}
fn usage(total: u64, uncached: Option<u64>) -> TokenUsage {
    TokenUsage {
        total_tokens: Some(total),
        usage_source: UsageSource::ProviderReported,
        cache_usage: CacheUsage {
            status: CacheUsageStatus::ProviderReported,
            uncached_input_tokens: uncached,
            ..Default::default()
        },
        ..Default::default()
    }
}

#[tokio::test]
async fn full_compaction_rechecks_model_admission() {
    for dimension in ["total", "uncached", "host"] {
        for summary_usage in [9, 10, 11] {
            let workspace = tempfile::tempdir().unwrap();
            let mut summary = LLMResponse::new(r#"{"progress":["done"]}"#);
            summary.token_usage = usage(summary_usage, Some(summary_usage));
            let mut primary = LLMResponse::new("done");
            primary.token_usage = usage(1, Some(1));
            let meter = CostMeter(Arc::new(AtomicU64::new(0)));
            let summary_meter = meter.clone();
            let runner = Runner::builder()
                .model_provider(
                    ScriptedModelProvider::from_steps(
                        "test",
                        "budget-model",
                        vec![
                            ScriptStep::callback(move |_| {
                                summary_meter.0.store(summary_usage, Ordering::SeqCst);
                                Ok(summary.clone())
                            }),
                            ScriptStep::response(primary),
                        ],
                    )
                    .with_token_limits(Some(10_000), Some(0)),
                )
                .build()
                .unwrap();
            let agent = Agent::builder("compaction-budget")
                .instructions("Return done.")
                .model(ModelRef::named("budget-model"))
                .no_tool_policy(NoToolPolicy::Finish)
                .build()
                .unwrap();
            let limits = match dimension {
                "host" => {
                    RunBudgetLimits::builder().max_host_cost(HostCost::new("credits", 10).unwrap())
                }
                "uncached" => RunBudgetLimits::builder().max_uncached_input_tokens(10),
                _ => RunBudgetLimits::builder().max_total_tokens(10),
            }
            .build()
            .unwrap();
            let mut config = RunConfig::builder()
                .workspace(workspace.path())
                .max_cycles(2)
                .budget_limits(limits)
                .metadata("memory_keep_recent_messages", serde_json::json!(1))
                .metadata("model_context_window", serde_json::json!(1000))
                .metadata("reserved_output_tokens", serde_json::json!(0))
                .metadata("autocompact_buffer_tokens", serde_json::json!(0))
                .initial_messages(vec![
                    vv_agent::Message::user("history ".repeat(1000)),
                    vv_agent::Message::assistant("previous step"),
                    vv_agent::Message::user("continue"),
                ])
                .build();
            if dimension == "host" {
                config.host_cost_meter = Some(Arc::new(meter));
            }
            let result = runner
                .run_with_config(&agent, "continue", config)
                .await
                .unwrap();
            let calls: Vec<_> = result
                .events()
                .iter()
                .filter_map(|event| match event.payload() {
                    vv_agent::RunEventPayload::ModelCallStarted { operation, .. } => {
                        Some(*operation)
                    }
                    _ => None,
                })
                .collect();
            let mut expected = vec![vv_agent::ModelCallOperation::MemoryCompaction];
            if summary_usage < 10 {
                expected.push(vv_agent::ModelCallOperation::AgentCycle);
            }
            assert_eq!(calls, expected);
            assert_eq!(result.budget_usage().unwrap().cycles, 1);
            if summary_usage < 10 {
                assert_eq!(result.status(), AgentStatus::Completed);
            } else {
                let exhaustion = result.budget_exhaustion().unwrap();
                assert_eq!(exhaustion.observed, Some(summary_usage));
                assert_eq!(
                    exhaustion.reason,
                    if summary_usage == 10 {
                        BudgetExhaustionReason::LimitReached
                    } else {
                        BudgetExhaustionReason::LimitExceeded
                    }
                );
            }
        }
    }
}
