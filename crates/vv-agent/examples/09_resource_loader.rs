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
        "workspace-loader",
        "你从 workspace 读取上下文文件、提取项目目标，并调用 task_finish 输出摘要。",
    )?;
    let prompt = config
        .prompt
        .unwrap_or_else(|| "读取 workspace 中可用的 README 或说明文件。".to_string());
    let result = runner
        .run_with_config(&agent, prompt, RunConfig::builder().max_cycles(8).build())
        .await?;
    print_run_result(&result)
}
