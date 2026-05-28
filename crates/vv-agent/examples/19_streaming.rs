use std::sync::{Arc, Mutex};

mod common;

use common::{
    build_direct_runtime, make_task_id, print_agent_result, runtime_log_handler, ExampleConfig,
};
use serde_json::Value;
use vv_agent::prompt::{build_system_prompt_with_options, BuildSystemPromptOptions};
use vv_agent::{AgentTask, ExecutionContext, RuntimeRunControls, StreamCallback};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    config.ensure_workspace()?;
    let (runtime, resolved) = build_direct_runtime(&config, 90.0)?;
    let collected = Arc::new(Mutex::new(Vec::<String>::new()));
    let callback_tokens = Arc::clone(&collected);
    let stream_callback: StreamCallback = Arc::new(move |event| {
        if event.get("event").and_then(Value::as_str) == Some("assistant_delta") {
            if let Some(delta) = event.get("content_delta").and_then(Value::as_str) {
                print!("{delta}");
                let _ = std::io::Write::flush(&mut std::io::stdout());
                callback_tokens
                    .lock()
                    .expect("stream callback token lock")
                    .push(delta.to_string());
            }
        }
    });

    let system_prompt = build_system_prompt_with_options(
        "You are a helpful agent. Answer concisely.",
        BuildSystemPromptOptions {
            language: "zh-CN".to_string(),
            allow_interruption: true,
            use_workspace: true,
            ..BuildSystemPromptOptions::default()
        },
    );
    let mut task = AgentTask::new(
        make_task_id("stream_demo"),
        resolved.model_id,
        system_prompt,
        config
            .prompt
            .unwrap_or_else(|| "用三句话介绍 Rust 语言".to_string()),
    );
    task.max_cycles = 5;

    println!("[demo] 流式输出开始:\n");
    let controls = RuntimeRunControls {
        workspace: Some(config.workspace),
        log_handler: runtime_log_handler(config.verbose),
        execution_context: Some(ExecutionContext::default().with_stream_callback(stream_callback)),
        ..RuntimeRunControls::default()
    };
    let result = runtime.run_with_controls(task, controls)?;
    println!(
        "\n\n[demo] 共收到 {} 个 token 片段",
        collected.lock().expect("stream token lock").len()
    );
    print_agent_result(&result)
}
