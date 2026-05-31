mod common;

use common::{build_facade_runner, print_run_result, ExampleConfig};
use serde::Deserialize;
use serde_json::json;
use vv_agent::{Agent, FunctionTool, ModelRef, ToolOutput};

#[derive(Deserialize)]
struct EchoArgs {
    text: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    config.ensure_workspace()?;
    let runner = build_facade_runner(&config)?;
    let echo = FunctionTool::builder("echo_uppercase")
        .description("Return the provided text uppercased.")
        .json_schema(json!({
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"]
        }))
        .handler(
            |_ctx, args: EchoArgs| async move { Ok(ToolOutput::text(args.text.to_uppercase())) },
        )
        .build()?;
    let agent = Agent::builder("tool-demo")
        .instructions("你必须调用 echo_uppercase，再用 task_finish 返回工具结果。")
        .model(ModelRef::backend(
            config.backend.clone(),
            config.model.clone(),
        ))
        .tool(echo)
        .build()?;
    let prompt = config
        .prompt
        .unwrap_or_else(|| "调用 echo_uppercase，text 为 vectorvein。".to_string());
    let result = runner.run(&agent, prompt).await?;
    print_run_result(&result)
}
