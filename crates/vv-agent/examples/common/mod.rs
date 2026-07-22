#![allow(dead_code)]

use std::env;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;
use vv_agent::types::AgentTask;
use vv_agent::{
    build_default_registry, build_vv_llm_from_local_settings, Agent, AgentRuntime, ModelRef,
    RunConfig, RunEvent, RunEventHandler, RunEventPayload, RunResult, Runner, RuntimeRunControls,
    VvLlmClient, VvLlmModelProvider,
};

pub struct ExampleConfig {
    pub settings_file: PathBuf,
    pub backend: String,
    pub model: String,
    pub workspace: PathBuf,
    pub prompt: Option<String>,
    pub verbose: bool,
}

impl ExampleConfig {
    pub fn load() -> Self {
        Self {
            settings_file: PathBuf::from(env_string(
                "VV_AGENT_LOCAL_SETTINGS",
                "local_settings.json",
            )),
            backend: env_string("VV_AGENT_EXAMPLE_BACKEND", "moonshot"),
            model: env_string("VV_AGENT_EXAMPLE_MODEL", "kimi-k3"),
            workspace: PathBuf::from(env_string("VV_AGENT_EXAMPLE_WORKSPACE", "./workspace")),
            prompt: env::var("VV_AGENT_EXAMPLE_PROMPT").ok(),
            verbose: env_bool("VV_AGENT_EXAMPLE_VERBOSE", true),
        }
    }

    pub fn ensure_workspace(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.workspace)
    }
}

pub fn env_string(name: &str, default: &str) -> String {
    env::var(name).unwrap_or_else(|_| default.to_string())
}

pub fn env_bool(name: &str, default: bool) -> bool {
    env::var(name)
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(default)
}

pub fn env_u32(name: &str, default: u32) -> u32 {
    env::var(name)
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(default)
}

pub fn env_u64(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(default)
}

pub fn env_usize(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(default)
}

pub fn env_f64(name: &str, default: f64) -> f64 {
    env::var(name)
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(default)
}

pub fn runtime_log_handler(verbose: bool) -> Option<RunEventHandler> {
    verbose.then(|| {
        Arc::new(move |event: &RunEvent| {
            if !matches!(
                event.payload,
                RunEventPayload::AssistantDelta { .. }
                    | RunEventPayload::ReasoningDelta { .. }
                    | RunEventPayload::ModelToolCallProgress { .. }
            ) {
                eprintln!("{}", serde_json::to_value(event).unwrap_or_default());
            }
        }) as RunEventHandler
    })
}

pub fn build_direct_runtime(
    config: &ExampleConfig,
    timeout_seconds: f64,
) -> Result<(AgentRuntime<VvLlmClient>, vv_agent::ResolvedModelConfig), Box<dyn std::error::Error>>
{
    let (llm, resolved) = build_vv_llm_from_local_settings(
        &config.settings_file,
        &config.backend,
        &config.model,
        timeout_seconds,
    )?;
    let runtime = AgentRuntime::new(llm).with_tool_registry(build_default_registry());
    Ok((runtime, resolved))
}

pub fn build_facade_runner(config: &ExampleConfig) -> Result<Runner, String> {
    let provider = VvLlmModelProvider::from_settings_file(config.settings_file.clone())
        .with_default_backend(config.backend.clone());
    Runner::builder()
        .model_provider(provider)
        .workspace(config.workspace.clone())
        .build()
}

pub fn build_facade_agent(
    config: &ExampleConfig,
    name: &str,
    instructions: &str,
) -> Result<Agent, String> {
    Agent::builder(name)
        .instructions(instructions)
        .model(ModelRef::backend(
            config.backend.clone(),
            config.model.clone(),
        ))
        .build()
}

pub async fn run_facade_prompt(
    config: &ExampleConfig,
    name: &str,
    instructions: &str,
    default_prompt: &str,
    run_config: RunConfig,
) -> Result<RunResult, Box<dyn std::error::Error>> {
    config.ensure_workspace()?;
    let prompt = config
        .prompt
        .clone()
        .unwrap_or_else(|| default_prompt.to_string());
    let runner = build_facade_runner(config)?;
    let agent = build_facade_agent(config, name, instructions)?;
    Ok(runner.run_with_config(&agent, prompt, run_config).await?)
}

pub fn run_direct_task(
    runtime: &AgentRuntime<VvLlmClient>,
    task: AgentTask,
    config: &ExampleConfig,
) -> Result<vv_agent::AgentResult, Box<dyn std::error::Error>> {
    let controls = RuntimeRunControls {
        workspace: Some(config.workspace.clone()),
        event_handler: runtime_log_handler(config.verbose),
        ..RuntimeRunControls::default()
    };
    runtime
        .run_with_controls(task, controls)
        .map_err(|error| error.into())
}

pub fn print_agent_result(
    result: &vv_agent::AgentResult,
) -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "status": format!("{:?}", result.status),
            "final_answer": result.final_answer,
            "wait_reason": result.wait_reason,
            "error": result.error,
            "cycles": result.cycles.len(),
            "token_usage": result.token_usage,
        }))?
    );
    Ok(())
}

pub fn print_run_result(result: &RunResult) -> Result<(), Box<dyn std::error::Error>> {
    let resolved = result.resolved_model();
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "agent": result.agent_name(),
            "status": format!("{:?}", result.status()),
            "final_output": result.final_output(),
            "wait_reason": result.result().wait_reason,
            "error": result.result().error,
            "cycles": result.result().cycles.len(),
            "token_usage": result.result().token_usage,
            "budget_usage": result.budget_usage(),
            "budget_exhaustion": result.budget_exhaustion(),
            "resolved": {
                "backend": resolved.map(|model| &model.backend),
                "selected_model": resolved.map(|model| &model.selected_model),
                "model_id": resolved.map(|model| &model.model_id),
            },
        }))?
    );
    Ok(())
}

pub fn make_task_id(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0);
    format!("{prefix}_{:08x}", (nanos ^ count) & 0xffff_ffff)
}
