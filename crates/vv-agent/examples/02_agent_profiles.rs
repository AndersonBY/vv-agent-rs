mod common;

use common::{build_facade_runner, print_run_result, ExampleConfig};
use vv_agent::{Agent, ModelRef};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    config.ensure_workspace()?;
    let runner = build_facade_runner(&config)?;
    let agent = Agent::builder("researcher")
        .instructions("你是研究型 Agent。先查阅 workspace，再给出结构化结论。")
        .model(ModelRef::backend(
            config.backend.clone(),
            config.model.clone(),
        ))
        .metadata("profile", serde_json::json!("researcher"))
        .build()?;
    let prompt = config
        .prompt
        .unwrap_or_else(|| "阅读 workspace 并总结当前项目定位。".to_string());
    let result = runner.run(&agent, prompt).await?;
    print_run_result(&result)
}
