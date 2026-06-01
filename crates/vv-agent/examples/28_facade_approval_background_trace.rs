mod common;

use std::sync::Arc;

use common::ExampleConfig;
use serde::Deserialize;
use serde_json::json;
use vv_agent::{
    Agent, ApprovalDecision, ApprovalFuture, ApprovalProvider, ApprovalRequest, FunctionTool,
    JsonlTraceExporter, ModelRef, RunConfig, RunEventPayload, Runner, ToolOutput,
    VvLlmModelProvider,
};

#[derive(Debug, Deserialize)]
struct EchoArgs {
    message: String,
}

struct HostApproval;

impl ApprovalProvider for HostApproval {
    fn should_request(&self, _request: &ApprovalRequest) -> bool {
        true
    }

    fn decide(&self, _request: &ApprovalRequest) -> ApprovalFuture<Option<ApprovalDecision>> {
        Box::pin(async { Ok(None) })
    }
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
        .build()?;
    let prompt = config
        .prompt
        .unwrap_or_else(|| "调用 approved_echo，message 为 approved path。".to_string());

    let handle = runner
        .start(
            &agent,
            prompt,
            RunConfig::builder()
                .approval_provider(Arc::new(HostApproval))
                .build(),
        )
        .await?;
    let mut events = handle.events();
    while let Some(event) = events.next().await {
        let event = event?;
        match event.payload() {
            RunEventPayload::ApprovalRequested {
                request_id,
                tool_name,
                preview,
                ..
            } => {
                println!("approval requested for {tool_name}: {preview}");
                let request_id = request_id.clone();
                handle
                    .approve(&request_id, ApprovalDecision::allow())
                    .await?;
            }
            RunEventPayload::ToolCallStarted { tool_name, .. } => {
                println!("tool started: {tool_name}");
            }
            RunEventPayload::RunCompleted { status } => {
                println!("run completed: {status:?}");
            }
            _ => {}
        }
    }
    let result = handle.result().await?;
    println!("{:?}: {:?}", result.status(), result.final_output());

    // Interrupted results can still be resumed with RunState::approve() when a
    // host chooses the older result/resume control flow.

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
