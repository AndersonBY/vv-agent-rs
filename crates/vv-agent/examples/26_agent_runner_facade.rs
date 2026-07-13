mod common;

use common::ExampleConfig;
use vv_agent::{Agent, ModelRef, RunConfig, Runner, VvLlmModelProvider};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    config.ensure_workspace()?;
    let prompt = config
        .prompt
        .clone()
        .unwrap_or_else(|| "读取 workspace README，并总结这个项目。".to_string());
    let provider = VvLlmModelProvider::from_settings_file(config.settings_file)
        .with_default_backend(config.backend.clone());
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(config.workspace)
        .build()?;
    let agent = Agent::builder("assistant")
        .instructions("你是可靠的执行型助手。先查证，再调用 task_finish 返回最终结果。")
        .model(ModelRef::backend(config.backend, config.model))
        .build()?;

    let result = runner
        .run_with_config(&agent, prompt, RunConfig::builder().max_cycles(12).build())
        .await?;
    let resolved = result.resolved_model();

    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "agent": result.agent_name(),
            "status": format!("{:?}", result.status()),
            "final_output": result.final_output(),
            "resolved": {
                "backend": resolved.map(|model| &model.backend),
                "selected_model": resolved.map(|model| &model.selected_model),
                "model_id": resolved.map(|model| &model.model_id),
            }
        }))?
    );
    Ok(())
}
