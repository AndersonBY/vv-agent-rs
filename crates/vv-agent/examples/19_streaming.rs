use std::io::Write;

mod common;

use common::{build_facade_agent, build_facade_runner, print_run_result, ExampleConfig};
use vv_agent::RunEventPayload;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    config.ensure_workspace()?;

    let runner = build_facade_runner(&config)?;
    let agent = build_facade_agent(
        &config,
        "stream-demo",
        "You are a helpful agent. Answer concisely.",
    )?;
    let prompt = config
        .prompt
        .unwrap_or_else(|| "用三句话介绍 Rust 语言".to_string());

    let mut stream = runner.stream(&agent, prompt).await?;
    let mut fragments = 0usize;

    println!("[demo] live event stream:\n");
    while let Some(event) = stream.next().await {
        let event = event?;
        match event.payload() {
            RunEventPayload::AssistantDelta { delta } => {
                print!("{delta}");
                std::io::stdout().flush()?;
                fragments += 1;
            }
            RunEventPayload::ToolCallStarted { tool_name, .. } => {
                eprintln!("\n[tool] started: {tool_name}");
            }
            RunEventPayload::ApprovalRequested {
                request_id,
                tool_name,
                ..
            } => {
                eprintln!("\n[approval] {tool_name} needs approval: {request_id}");
            }
            RunEventPayload::RunCompleted { status } => {
                eprintln!("\n[run] completed: {status:?}");
            }
            _ => {}
        }
    }

    println!("\n\n[demo] received {fragments} assistant fragments");
    let result = stream.into_result().await?;
    print_run_result(&result)
}
