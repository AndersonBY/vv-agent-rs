#![allow(deprecated)]

use std::sync::{Arc, Mutex};

mod common;

use common::{print_run, runtime_log_handler, ExampleConfig};
use serde_json::Value;
use vv_agent::{AgentDefinition, AgentSDKClient, AgentSDKOptions, StreamCallback, ThreadBackend};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    config.ensure_workspace()?;
    let token_count = Arc::new(Mutex::new(0usize));
    let counter = Arc::clone(&token_count);
    let stream_callback: StreamCallback = Arc::new(move |event| {
        if event.get("event").and_then(Value::as_str) == Some("assistant_delta") {
            if let Some(delta) = event.get("content_delta").and_then(Value::as_str) {
                print!("{delta}");
                let _ = std::io::Write::flush(&mut std::io::stdout());
                *counter.lock().expect("token count lock") += 1;
            }
        }
    });

    let mut agent = AgentDefinition::default_for_model(config.model.clone());
    agent.description = "你是一个简洁的助手, 用最少的话回答问题.".to_string();
    agent.backend = Some(config.backend.clone());
    agent.max_cycles = 5;

    let client = AgentSDKClient::new_with_agent(
        AgentSDKOptions {
            settings_file: config.settings_file,
            default_backend: config.backend,
            workspace: config.workspace,
            log_handler: runtime_log_handler(config.verbose),
            execution_backend: Some(ThreadBackend::new(2).into()),
            stream_callback: Some(stream_callback),
            ..AgentSDKOptions::default()
        },
        agent,
    );
    let prompt = config
        .prompt
        .unwrap_or_else(|| "什么是量子计算? 三句话回答".to_string());
    println!("[demo] 提问: {prompt}\n\n[demo] 流式输出:\n");
    let run = client.run(prompt)?;
    println!(
        "\n\n[demo] 共收到 {} 个 token 片段",
        *token_count.lock().expect("token count lock")
    );
    print_run(&run)
}
