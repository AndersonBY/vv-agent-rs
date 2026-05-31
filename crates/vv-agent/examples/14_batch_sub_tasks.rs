mod common;

use common::{build_facade_runner, print_run_result, ExampleConfig};
use vv_agent::{Agent, ModelRef};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    config.ensure_workspace()?;
    let runner = build_facade_runner(&config)?;
    let summarizer = Agent::builder("summarizer")
        .instructions("你是摘要 Agent。输入一个文档任务，输出三条要点。")
        .model(ModelRef::backend(
            config.backend.clone(),
            config.model.clone(),
        ))
        .build()?;
    let summarizer_tool = summarizer
        .as_tool()
        .name("summarize_document")
        .description("Summarize one delegated document task.")
        .build()?;
    let coordinator = Agent::builder("batch-coordinator")
        .instructions("你会把多个文档摘要任务交给 summarize_document，然后汇总。")
        .model(ModelRef::backend(
            config.backend.clone(),
            config.model.clone(),
        ))
        .tool(summarizer_tool)
        .max_cycles(12)
        .build()?;
    let prompt = config
        .prompt
        .unwrap_or_else(|| "从 workspace 中选择最多三个文档，分别摘要后汇总。".to_string());
    let result = runner.run(&coordinator, prompt).await?;
    print_run_result(&result)
}
