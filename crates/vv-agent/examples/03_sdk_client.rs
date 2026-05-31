mod common;

use common::{build_facade_runner, print_run_result, ExampleConfig};
use vv_agent::{handoff, Agent, ModelRef, RunConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    config.ensure_workspace()?;
    let runner = build_facade_runner(&config)?;
    let translator = Agent::builder("translator")
        .instructions("你是翻译 Agent。把输入翻译成清晰中文，并保留技术名词。")
        .model(ModelRef::backend(
            config.backend.clone(),
            config.model.clone(),
        ))
        .build()?;
    let orchestrator = Agent::builder("orchestrator")
        .instructions("你是主控 Agent。需要翻译时转交 translator，否则直接完成。")
        .model(ModelRef::backend(
            config.backend.clone(),
            config.model.clone(),
        ))
        .handoff(handoff(&translator).description("需要翻译输入时使用"))
        .build()?;
    let prompt = config
        .prompt
        .unwrap_or_else(|| "Translate: Build a reliable multi-agent runtime.".to_string());
    let result = runner
        .run_with_config(
            &orchestrator,
            prompt,
            RunConfig::builder().max_cycles(8).build(),
        )
        .await?;
    print_run_result(&result)
}
