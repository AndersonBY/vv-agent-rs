#![allow(dead_code)]

use std::collections::BTreeMap;
use std::env;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Value};
use vv_agent::{
    build_default_registry, build_vv_llm_from_local_settings, AgentRuntime, AgentTask,
    RuntimeEventHandler, RuntimeRunControls, VvLlmClient,
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
            backend: env_string("V_AGENT_EXAMPLE_BACKEND", "moonshot"),
            model: env_string("V_AGENT_EXAMPLE_MODEL", "kimi-k2.5"),
            workspace: PathBuf::from(env_string("V_AGENT_EXAMPLE_WORKSPACE", "./workspace")),
            prompt: env::var("V_AGENT_EXAMPLE_PROMPT").ok(),
            verbose: env_bool("V_AGENT_EXAMPLE_VERBOSE", true),
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

pub fn runtime_log_handler(verbose: bool) -> Option<RuntimeEventHandler> {
    verbose.then(|| {
        Arc::new(move |event: &str, payload: &BTreeMap<String, Value>| {
            if matches!(
                event,
                "run_started"
                    | "cycle_started"
                    | "cycle_llm_response"
                    | "tool_result"
                    | "run_completed"
                    | "run_wait_user"
                    | "run_max_cycles"
                    | "cycle_failed"
                    | "run_cancelled"
            ) {
                eprintln!(
                    "[{event}] {}",
                    Value::Object(payload.clone().into_iter().collect())
                );
            }
        }) as RuntimeEventHandler
    })
}

pub fn session_log_handler(verbose: bool) -> RuntimeEventHandler {
    Arc::new(move |event: &str, payload: &BTreeMap<String, Value>| {
        if verbose
            && matches!(
                event,
                "session_run_start"
                    | "cycle_started"
                    | "cycle_llm_response"
                    | "tool_result"
                    | "run_wait_user"
                    | "run_completed"
                    | "session_run_end"
                    | "session_steer_queued"
                    | "session_follow_up_queued"
            )
        {
            eprintln!(
                "[{event}] {}",
                Value::Object(payload.clone().into_iter().collect())
            );
        }
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

pub fn run_direct_task(
    runtime: &AgentRuntime<VvLlmClient>,
    task: AgentTask,
    config: &ExampleConfig,
) -> Result<vv_agent::AgentResult, Box<dyn std::error::Error>> {
    let controls = RuntimeRunControls {
        workspace: Some(config.workspace.clone()),
        log_handler: runtime_log_handler(config.verbose),
        ..RuntimeRunControls::default()
    };
    runtime
        .run_with_controls(task, controls)
        .map_err(|error| error.into())
}

pub fn print_run(run: &vv_agent::AgentRun) -> Result<(), Box<dyn std::error::Error>> {
    println!("{}", serde_json::to_string_pretty(&run.to_dict())?);
    Ok(())
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
