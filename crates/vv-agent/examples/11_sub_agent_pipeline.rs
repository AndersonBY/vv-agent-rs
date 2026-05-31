mod common;

use common::{build_facade_runner, print_run_result, ExampleConfig};
use vv_agent::{Agent, ModelRef};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    config.ensure_workspace()?;
    let runner = build_facade_runner(&config)?;
    let reviewer = Agent::builder("reviewer")
        .instructions("你审阅输入并返回关键风险。")
        .model(ModelRef::backend(
            config.backend.clone(),
            config.model.clone(),
        ))
        .build()?;
    let reviewer_tool = reviewer
        .as_tool()
        .name("review_with_agent")
        .description("Run the reviewer agent on a delegated task.")
        .build()?;
    let coordinator = Agent::builder("coordinator")
        .instructions("你是项目总控 Agent。先调用 review_with_agent，再整合最终答案。")
        .model(ModelRef::backend(
            config.backend.clone(),
            config.model.clone(),
        ))
        .tool(reviewer_tool)
        .build()?;
    let prompt = config
        .prompt
        .unwrap_or_else(|| "审阅 workspace 中的产品说明并汇总风险。".to_string());
    let result = runner.run(&coordinator, prompt).await?;
    print_run_result(&result)
}
