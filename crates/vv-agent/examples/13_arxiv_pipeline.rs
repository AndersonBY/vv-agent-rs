mod common;

use common::{print_run_result, run_facade_prompt, ExampleConfig};
use vv_agent::{ModelSettings, RunConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    let result = run_facade_prompt(
        &config,
        "research-pipeline",
        "你是研究型 Agent。搜索资料、整理引用、写出结论，并调用 task_finish。",
        "围绕 AI Agent Memory 做一次简短研究，输出要点和后续阅读建议。",
        RunConfig::builder()
            .model_settings(ModelSettings::builder().temperature(0.2).build())
            .max_cycles(12)
            .build(),
    )
    .await?;
    print_run_result(&result)
}
