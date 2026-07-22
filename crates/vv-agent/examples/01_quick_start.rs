use std::env;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;
use vv_agent::config::build_vv_llm_from_local_settings;
use vv_agent::prompt::{build_system_prompt_with_options, BuildSystemPromptOptions};
use vv_agent::types::AgentTask;
use vv_agent::{
    build_default_registry, AgentRuntime, RunEvent, RunEventHandler, RuntimeRunControls,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let settings_file =
        env::var("VV_AGENT_LOCAL_SETTINGS").unwrap_or_else(|_| "local_settings.json".to_string());
    let backend = env::var("VV_AGENT_EXAMPLE_BACKEND").unwrap_or_else(|_| "moonshot".to_string());
    let model = env::var("VV_AGENT_EXAMPLE_MODEL").unwrap_or_else(|_| "kimi-k3".to_string());
    let workspace = PathBuf::from(
        env::var("VV_AGENT_EXAMPLE_WORKSPACE").unwrap_or_else(|_| "./workspace".to_string()),
    )
    .canonicalize()
    .unwrap_or_else(|_| PathBuf::from("./workspace"));
    let prompt = env::var("VV_AGENT_EXAMPLE_PROMPT")
        .unwrap_or_else(|_| "请概述一下这个框架的特点".to_string());
    let max_cycles = env::var("VV_AGENT_EXAMPLE_MAX_CYCLES")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(10)
        .max(1);
    let verbose = env::var("VV_AGENT_EXAMPLE_VERBOSE")
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(true);

    std::fs::create_dir_all(&workspace)?;

    let (llm, resolved) = build_vv_llm_from_local_settings(&settings_file, &backend, &model, 90.0)?;
    let runtime = AgentRuntime::new(llm).with_tool_registry(build_default_registry());

    let system_prompt = build_system_prompt_with_options(
        "You are a reliable execution agent. Use tools explicitly and give clear final outputs.",
        BuildSystemPromptOptions {
            language: "zh-CN".to_string(),
            allow_interruption: true,
            use_workspace: true,
            enable_todo_management: true,
            ..BuildSystemPromptOptions::default()
        },
    );

    let mut task = AgentTask::new(
        "quickstart",
        resolved.model_id.clone(),
        system_prompt,
        prompt,
    );
    task.max_cycles = max_cycles;
    let controls = RuntimeRunControls {
        workspace: Some(workspace),
        event_handler: verbose.then(|| Arc::new(log_handler()) as RunEventHandler),
        ..RuntimeRunControls::default()
    };

    let result = runtime.run_with_controls(task, controls)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "status": format!("{:?}", result.status),
            "final_answer": result.final_answer,
            "wait_reason": result.wait_reason,
            "error": result.error,
            "cycles": result.cycles.len(),
            "resolved": {
                "backend": resolved.backend,
                "selected_model": resolved.selected_model,
                "model_id": resolved.model_id,
                "endpoint": resolved.endpoint().map(|endpoint| endpoint.endpoint_id.clone()),
            },
        }))?
    );
    Ok(())
}

fn log_handler() -> impl Fn(&RunEvent) + Send + Sync + 'static {
    |event| {
        eprintln!("{}", serde_json::to_value(event).unwrap_or_default());
    }
}
