mod common;

use std::sync::Arc;

use common::{print_run_result, run_facade_prompt, ExampleConfig};
use vv_agent::{
    AfterToolCallEvent, BeforeToolCallEvent, BeforeToolCallPatch, RunConfig, RuntimeHook,
    ToolExecutionResult,
};

struct ToolPolicyHook;

impl RuntimeHook for ToolPolicyHook {
    fn before_tool_call(&self, event: BeforeToolCallEvent<'_>) -> Option<BeforeToolCallPatch> {
        if event.call.name == "bash" && event.call.arguments.contains_key("command") {
            return None;
        }
        None
    }

    fn after_tool_call(&self, event: AfterToolCallEvent<'_>) -> Option<ToolExecutionResult> {
        eprintln!("[tool] {} -> {:?}", event.call.name, event.result.status);
        None
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    let result = run_facade_prompt(
        &config,
        "composed-hooks",
        "你是文件处理 Agent。读取 workspace 文件，并输出摘要。",
        "读取 workspace 的 README 或任意说明文件并总结。",
        RunConfig::builder()
            .hook(Arc::new(ToolPolicyHook))
            .max_cycles(8)
            .build(),
    )
    .await?;
    print_run_result(&result)
}
