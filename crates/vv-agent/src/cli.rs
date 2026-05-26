use std::collections::BTreeMap;
use std::env;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{SecondsFormat, Utc};
use serde_json::{json, Value};

use crate::config::{build_vv_llm_from_local_settings, ResolvedModelConfig};
use crate::prompt::{build_system_prompt_bundle_with_options, BuildSystemPromptOptions};
use crate::runtime::{AgentRuntime, RuntimeLogCallback};
use crate::types::{AgentResult, AgentStatus, AgentTask};
use crate::workspace::LocalWorkspaceBackend;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliArgs {
    pub prompt: String,
    pub backend: String,
    pub model: String,
    pub settings_file: PathBuf,
    pub workspace: PathBuf,
    pub max_cycles: u32,
    pub language: String,
    pub agent_type: Option<String>,
    pub verbose: bool,
}

pub fn parse_cli_args_from<I, S>(args: I) -> Result<CliArgs, String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let default_settings =
        env::var("V_AGENT_LOCAL_SETTINGS").unwrap_or_else(|_| "local_settings.py".to_string());
    parse_cli_args_from_with_default_settings(args, default_settings)
}

pub fn parse_cli_args_from_with_default_settings<I, S>(
    args: I,
    default_settings: impl Into<String>,
) -> Result<CliArgs, String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut values = args.into_iter().map(Into::into).collect::<Vec<_>>();
    if !values.is_empty() {
        values.remove(0);
    }

    let mut prompt = None::<String>;
    let mut backend = "moonshot".to_string();
    let mut model = "kimi-k2.5".to_string();
    let mut settings_file = PathBuf::from(default_settings.into());
    let mut workspace = PathBuf::from("./workspace");
    let mut max_cycles = 80_u32;
    let mut language = "zh-CN".to_string();
    let mut agent_type = None::<String>;
    let mut verbose = false;

    let mut index = 0;
    while index < values.len() {
        let flag = &values[index];
        index += 1;
        match flag.as_str() {
            "--prompt" => prompt = Some(next_value(&values, &mut index, "--prompt")?),
            "--backend" => backend = next_value(&values, &mut index, "--backend")?,
            "--model" => model = next_value(&values, &mut index, "--model")?,
            "--settings-file" => {
                settings_file = PathBuf::from(next_value(&values, &mut index, "--settings-file")?)
            }
            "--workspace" => {
                workspace = PathBuf::from(next_value(&values, &mut index, "--workspace")?)
            }
            "--max-cycles" => {
                let raw = next_value(&values, &mut index, "--max-cycles")?;
                max_cycles = raw
                    .parse::<u32>()
                    .map_err(|_| "--max-cycles must be an integer".to_string())?
                    .max(1);
            }
            "--language" => language = next_value(&values, &mut index, "--language")?,
            "--agent-type" => {
                agent_type = Some(next_value(&values, &mut index, "--agent-type")?)
                    .filter(|value| !value.trim().is_empty())
            }
            "--verbose" => verbose = true,
            "--help" | "-h" => return Err(help_text()),
            other => return Err(format!("unknown argument: {other}\n\n{}", help_text())),
        }
    }

    let Some(prompt) = prompt.filter(|value| !value.trim().is_empty()) else {
        return Err(format!("--prompt is required\n\n{}", help_text()));
    };

    Ok(CliArgs {
        prompt,
        backend,
        model,
        settings_file,
        workspace,
        max_cycles,
        language,
        agent_type,
        verbose,
    })
}

pub fn build_cli_task(
    args: &CliArgs,
    model_id: impl Into<String>,
    task_id: impl Into<String>,
) -> Result<AgentTask, String> {
    let prompt_bundle = build_system_prompt_bundle_with_options(
        "You are Vector Vein agent runtime demo. Execute tasks with reliable tool usage and clear final outputs.",
        BuildSystemPromptOptions {
            language: args.language.clone(),
            allow_interruption: true,
            use_workspace: true,
            enable_todo_management: true,
            agent_type: args.agent_type.clone(),
            workspace: Some(args.workspace.clone()),
            ..BuildSystemPromptOptions::default()
        },
    );
    let mut task = AgentTask::new(task_id, model_id, prompt_bundle.prompt, args.prompt.clone());
    task.max_cycles = args.max_cycles.max(1);
    task.agent_type = args.agent_type.clone();
    task.metadata
        .insert("language".to_string(), Value::String(args.language.clone()));
    task.metadata.insert(
        "system_prompt_sections".to_string(),
        Value::Array(prompt_bundle.sections),
    );
    Ok(task)
}

pub fn result_payload(result: &AgentResult, resolved: &ResolvedModelConfig) -> Value {
    json!({
        "status": status_value(result.status),
        "final_answer": result.final_answer,
        "wait_reason": result.wait_reason,
        "error": result.error,
        "cycles": result.cycles.len(),
        "todo_list": result.todo_list(),
        "resolved": {
            "backend": resolved.backend,
            "selected_model": resolved.selected_model,
            "model_id": resolved.model_id,
            "endpoint": resolved.endpoint().map(|endpoint| endpoint.endpoint_id.clone()),
        },
    })
}

pub fn main() -> Result<(), String> {
    let args = parse_cli_args_from(env::args())?;
    let (llm, resolved) =
        build_vv_llm_from_local_settings(&args.settings_file, &args.backend, &args.model, 90.0)
            .map_err(|err| err.to_string())?;

    let mut runtime = AgentRuntime::new(llm)
        .with_settings_file(args.settings_file.clone())
        .with_default_backend(args.backend.clone());
    runtime.default_workspace = Some(args.workspace.clone());
    runtime.workspace_backend = Arc::new(LocalWorkspaceBackend::new(args.workspace.clone()));
    runtime.log_handler = build_cli_log_handler(args.verbose);

    let task = build_cli_task(&args, resolved.model_id.clone(), generate_task_id())?;
    let result = runtime.run(task).map_err(|err| err.to_string())?;
    let payload = result_payload(&result, &resolved);
    let output = serde_json::to_string_pretty(&payload).map_err(|err| err.to_string())?;
    println!("{output}");
    Ok(())
}

fn next_value(values: &[String], index: &mut usize, flag: &str) -> Result<String, String> {
    let Some(value) = values.get(*index) else {
        return Err(format!("{flag} requires a value"));
    };
    *index += 1;
    Ok(value.clone())
}

fn help_text() -> String {
    [
        "Run a vv-agent task against a configured LLM endpoint.",
        "",
        "Required:",
        "  --prompt <text>",
        "",
        "Options:",
        "  --backend <key>        Provider backend key in LLM_SETTINGS (default: moonshot)",
        "  --model <key>          Model key in provider models (default: kimi-k2.5)",
        "  --settings-file <path> Path to local settings (default: V_AGENT_LOCAL_SETTINGS or local_settings.py)",
        "  --workspace <path>     Workspace directory (default: ./workspace)",
        "  --max-cycles <n>       Max runtime cycles (default: 80)",
        "  --language <locale>    System prompt language (default: zh-CN)",
        "  --agent-type <type>    Agent type, e.g. computer",
        "  --verbose             Show per-cycle runtime logs",
    ]
    .join("\n")
}

fn status_value(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Pending => "pending",
        AgentStatus::Running => "running",
        AgentStatus::WaitUser => "wait_user",
        AgentStatus::Completed => "completed",
        AgentStatus::Failed => "failed",
        AgentStatus::MaxCycles => "max_cycles",
    }
}

fn generate_task_id() -> String {
    format!("task_{}", Utc::now().format("%Y%m%d%H%M%S%3f"))
}

fn build_cli_log_handler(enabled: bool) -> Option<Arc<Mutex<Box<RuntimeLogCallback>>>> {
    if !enabled {
        return None;
    }
    let handler: Box<RuntimeLogCallback> = Box::new(|event, payload| {
        let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        let line = format_cli_log_line(&now, event, payload);
        eprintln!("{line}");
    });
    Some(Arc::new(Mutex::new(handler)))
}

fn format_cli_log_line(now: &str, event: &str, payload: &BTreeMap<String, Value>) -> String {
    match event {
        "run_started" => format!(
            "[{now}] [run] start task={} model={} max_cycles={}",
            payload_text(payload, "task_id"),
            payload_text(payload, "model"),
            payload_text(payload, "max_cycles")
        ),
        "cycle_started" => format!(
            "[{now}] [cycle {}] start messages={}",
            payload_text(payload, "cycle"),
            payload_text(payload, "message_count")
        ),
        "cycle_llm_response" => format!(
            "[{now}] [cycle {}] llm tool_calls={} assistant={}",
            payload_text(payload, "cycle"),
            payload_text(payload, "tool_call_names"),
            payload_text(payload, "assistant_preview")
        ),
        "tool_result" => format!(
            "[{now}] [cycle {}] tool={} status={} directive={} preview={}",
            payload_text(payload, "cycle"),
            payload_text(payload, "tool_name"),
            payload_text(payload, "status"),
            payload_text(payload, "directive"),
            payload_text(payload, "content_preview")
        ),
        "run_completed" => format!(
            "[{now}] [run] completed: {}",
            payload_text(payload, "final_answer")
        ),
        "run_wait_user" => format!(
            "[{now}] [run] wait_user: {}",
            payload_text(payload, "wait_reason")
        ),
        "run_max_cycles" => format!("[{now}] [run] max_cycles reached"),
        "cycle_failed" => format!(
            "[{now}] [cycle {}] failed: {}",
            payload_text(payload, "cycle"),
            payload_text(payload, "error")
        ),
        other => format!(
            "[{now}] [{other}] {}",
            Value::Object(payload.clone().into_iter().collect())
        ),
    }
}

fn payload_text(payload: &BTreeMap<String, Value>, key: &str) -> String {
    payload
        .get(key)
        .map(|value| match value {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        })
        .unwrap_or_default()
}
