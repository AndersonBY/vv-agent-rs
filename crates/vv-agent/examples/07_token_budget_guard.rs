mod common;

use common::{print_run_result, run_facade_prompt, ExampleConfig};
use vv_agent::{ModelSettings, RunConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    let result = run_facade_prompt(
        &config,
        "budgeted-agent",
        "你是受预算约束的执行 Agent。保持步骤简洁，尽早调用 task_finish。",
        "用不超过五点说明当前 workspace 的主要内容。",
        RunConfig::builder()
            .model_settings(ModelSettings::builder().max_output_tokens(800).build())
            .max_cycles(5)
            .build(),
    )
    .await?;
    print_run_result(&result)
}
