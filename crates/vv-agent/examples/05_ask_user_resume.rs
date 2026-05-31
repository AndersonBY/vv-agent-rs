mod common;

use common::{print_run_result, run_facade_prompt, ExampleConfig};
use vv_agent::RunConfig;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    let result = run_facade_prompt(
        &config,
        "interactive-writer",
        "你是交互式写作 Agent。缺少关键偏好时先调用 ask_user；信息足够后调用 task_finish。",
        "先询问写作风格和目标读者，再给出文章提纲。",
        RunConfig::builder().max_cycles(8).build(),
    )
    .await?;
    print_run_result(&result)
}
