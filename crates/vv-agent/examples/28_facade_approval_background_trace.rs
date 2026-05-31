mod common;

use std::sync::Arc;

use common::ExampleConfig;
use serde::Deserialize;
use serde_json::json;
use vv_agent::{
    Agent, ApprovalPolicy, FunctionTool, JsonlTraceExporter, ModelRef, RunConfig, Runner,
    ToolOutput, ToolPolicy, VvLlmModelProvider,
};

#[derive(Debug, Deserialize)]
struct EchoArgs {
    message: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    config.ensure_workspace()?;
    let provider = VvLlmModelProvider::from_settings_file(config.settings_file)
        .with_default_backend(config.backend.clone());
    let trace_path = config.workspace.join("agent-trace.jsonl");
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(config.workspace.clone())
        .default_run_config(
            RunConfig::builder()
                .trace_sink(Arc::new(JsonlTraceExporter::new(&trace_path)?))
                .build(),
        )
        .build()?;
    let echo = FunctionTool::builder("approved_echo")
        .description("Echo a message after approval.")
        .json_schema(json!({
            "type": "object",
            "properties": {"message": {"type": "string"}},
            "required": ["message"]
        }))
        .handler(|_ctx, args: EchoArgs| async move { Ok(ToolOutput::text(args.message)) })
        .build()?;
    let agent = Agent::builder("approval-demo")
        .instructions("调用 approved_echo 前会等待宿主审批，获批后再完成。")
        .model(ModelRef::backend(config.backend, config.model))
        .tool(echo)
        .tool_policy(ToolPolicy {
            approval: ApprovalPolicy::Always,
            ..ToolPolicy::default()
        })
        .build()?;
    let prompt = config
        .prompt
        .unwrap_or_else(|| "调用 approved_echo，message 为 approved path。".to_string());

    let first = runner.run(&agent, prompt).await?;
    if first.status() == vv_agent::AgentStatus::WaitUser {
        let mut state = first.into_state()?;
        if let Some(interruption_id) = state.pending_approval_ids().first().cloned() {
            state.approve(&interruption_id)?;
            let resumed = runner.resume(state).await?;
            println!("{:?}: {:?}", resumed.status(), resumed.final_output());
        }
    }

    let background = agent
        .as_background_task()
        .name("approval_demo_background")
        .build()?;
    let mut context = vv_agent::ToolContext::new(config.workspace);
    let handle = background.start(
        &runner,
        &mut context,
        json!({"task_description": "后台执行同一个 approval demo agent"}),
    )?;
    println!("background task {} {:?}", handle.task_id(), handle.status());
    println!("trace: {}", trace_path.display());
    Ok(())
}
