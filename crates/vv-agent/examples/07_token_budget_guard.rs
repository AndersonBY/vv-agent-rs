mod common;

use common::{env_u64, print_run_result, run_facade_prompt, ExampleConfig};
use vv_agent::{RunBudgetLimits, RunConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    let budget_limits = RunBudgetLimits::builder()
        .max_total_tokens(env_u64("VV_AGENT_EXAMPLE_TOKEN_BUDGET", 4_000))
        .max_tool_calls(env_u64("VV_AGENT_EXAMPLE_TOOL_BUDGET", 12))
        .build()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    let result = run_facade_prompt(
        &config,
        "budgeted-agent",
        "Keep the answer concise and call task_finish when the work is complete.",
        "Summarize how Agent run budgets work.",
        RunConfig::builder().budget_limits(budget_limits).build(),
    )
    .await?;
    print_run_result(&result)
}
