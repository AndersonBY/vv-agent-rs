mod common;

use common::ExampleConfig;
use vv_agent::{handoff, Agent, ModelRef, Runner, VvLlmModelProvider};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    config.ensure_workspace()?;
    let provider = VvLlmModelProvider::from_settings_file(config.settings_file)
        .with_default_backend(config.backend.clone());
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(config.workspace)
        .build()?;
    let researcher = Agent::builder("researcher")
        .instructions("你负责收集事实并调用 task_finish 输出简洁结论。")
        .model(ModelRef::backend(
            config.backend.clone(),
            config.model.clone(),
        ))
        .build()?;
    let triage = Agent::builder("triage")
        .instructions("你负责判断任务类型，需要研究时转交给 researcher。")
        .model(ModelRef::backend(config.backend, config.model))
        .handoff(handoff(&researcher).description("需要事实收集时使用"))
        .build()?;
    let prompt = config
        .prompt
        .unwrap_or_else(|| "研究这个 workspace 的项目定位。".to_string());

    let result = runner.run(&triage, prompt).await?;

    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "agent": result.agent_name(),
            "status": format!("{:?}", result.status()),
            "final_output": result.final_output(),
        }))?
    );
    Ok(())
}
