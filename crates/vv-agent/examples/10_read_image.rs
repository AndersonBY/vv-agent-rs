mod common;

use common::{print_run_result, run_facade_prompt, ExampleConfig};
use vv_agent::RunConfig;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    let result = run_facade_prompt(
        &config,
        "vision-reader",
        "你可以使用 read_image 工具读取 workspace 图片，然后用 task_finish 总结图片内容。",
        "如果 workspace 里有图片，请读取并描述；否则说明需要提供图片路径。",
        RunConfig::builder().max_cycles(8).build(),
    )
    .await?;
    print_run_result(&result)
}
