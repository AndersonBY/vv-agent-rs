use chrono::Utc;
use serde_json::Value;

use crate::config::{apply_resolved_model_limits, ResolvedModelConfig};
use crate::prompt::{build_system_prompt_bundle_with_options, BuildSystemPromptOptions};
use crate::types::AgentTask;

use super::args::CliArgs;

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

pub fn build_cli_task_from_resolved(
    args: &CliArgs,
    resolved: &ResolvedModelConfig,
    task_id: impl Into<String>,
) -> Result<AgentTask, String> {
    let mut task = build_cli_task(args, resolved.model_id.clone(), task_id)?;
    apply_resolved_model_limits(&mut task, resolved);
    Ok(task)
}

pub(super) fn generate_task_id() -> String {
    format!("task_{}", Utc::now().format("%Y%m%d%H%M%S%3f"))
}
