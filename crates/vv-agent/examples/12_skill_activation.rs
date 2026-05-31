mod common;

use common::{print_run_result, run_facade_prompt, ExampleConfig};
use vv_agent::RunConfig;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    let result = run_facade_prompt(
        &config,
        "skill-agent",
        "你会先查看可用 skill，并在任务需要时调用 activate_skill。",
        "检查是否有适合代码审查或实现规划的 skill，并说明你的选择。",
        RunConfig::builder().max_cycles(8).build(),
    )
    .await?;
    print_run_result(&result)
}
