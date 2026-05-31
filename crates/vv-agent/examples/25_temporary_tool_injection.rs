mod common;

use common::{build_facade_runner, print_run_result, ExampleConfig};
use serde_json::json;
use vv_agent::{Agent, FunctionTool, ModelRef, ToolOutput};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    config.ensure_workspace()?;
    let runner = build_facade_runner(&config)?;
    let context_tool = FunctionTool::builder("temporary_context")
        .description("Return temporary runtime context for this run.")
        .json_schema(json!({"type": "object", "properties": {}, "required": []}))
        .handler(|_ctx, _args: serde_json::Value| async move {
            Ok(ToolOutput::text(
                "temporary context is available for this run",
            ))
        })
        .build()?;
    let agent = Agent::builder("temporary-tool-agent")
        .instructions("你必须先调用 temporary_context，再结合其结果调用 task_finish。")
        .model(ModelRef::backend(
            config.backend.clone(),
            config.model.clone(),
        ))
        .tool(context_tool)
        .build()?;
    let prompt = config
        .prompt
        .unwrap_or_else(|| "读取 temporary_context 并说明它的用途。".to_string());
    let result = runner.run(&agent, prompt).await?;
    print_run_result(&result)
}
