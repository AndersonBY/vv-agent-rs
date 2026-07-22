mod common;

use common::{print_run_result, run_facade_prompt, ExampleConfig};
use vv_agent::{ExecutionMode, ModelSettings, RunConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    let result = run_facade_prompt(
        &config,
        "advanced-runner",
        "你是高级运行配置示例 Agent。说明你使用的执行策略并完成任务。",
        "用三点说明 Runner 的高级配置能力。",
        RunConfig::builder()
            .execution_mode(ExecutionMode::Threaded { max_workers: 2 })
            .model_settings(
                ModelSettings::builder()
                    .temperature(0.3)
                    .max_tokens(1200)
                    .build(),
            )
            .max_cycles(8)
            .build(),
    )
    .await?;
    print_run_result(&result)
}
