mod common;

use common::{build_facade_agent, build_facade_runner, print_run_result, ExampleConfig};
use vv_agent::RunConfig;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    config.ensure_workspace()?;
    let runner = build_facade_runner(&config)?;
    let agent = build_facade_agent(
        &config,
        "retrying-agent",
        "你是可靠执行 Agent。遇到可恢复问题时重新组织步骤并调用 task_finish。",
    )?;
    let prompt = config
        .prompt
        .unwrap_or_else(|| "总结 workspace，失败时用更短步骤重试一次。".to_string());
    let mut last_error = None;
    for attempt in 1..=2 {
        match runner
            .run_with_config(
                &agent,
                prompt.clone(),
                RunConfig::builder()
                    .metadata("attempt", attempt.into())
                    .build(),
            )
            .await
        {
            Ok(result) => return print_run_result(&result),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error
        .unwrap_or_else(|| "run failed".to_string())
        .into())
}
