use serde_json::Value;

use crate::constants::{
    ACTIVATE_SKILL_TOOL_NAME, ASK_USER_TOOL_NAME, BASH_TOOL_NAME,
    CHECK_BACKGROUND_COMMAND_TOOL_NAME, CREATE_SUB_TASK_TOOL_NAME, READ_IMAGE_TOOL_NAME,
    SUB_TASK_STATUS_TOOL_NAME, TASK_FINISH_TOOL_NAME, WORKSPACE_TOOLS,
};
use crate::tools::ToolRegistry;
use crate::types::AgentTask;

use super::shell::{normalize_windows_shell_priority, resolve_shell_invocation};

const BASH_RUNTIME_HINT_METADATA_KEY: &str = "_vv_agent_bash_runtime_hint";

pub fn plan_tool_names(task: &AgentTask, memory_usage_percentage: Option<u32>) -> Vec<String> {
    let _ = memory_usage_percentage;
    let mut names = vec![TASK_FINISH_TOOL_NAME.to_string()];
    if task.allow_interruption {
        names.push(ASK_USER_TOOL_NAME.to_string());
    }
    if task.use_workspace {
        names.extend(WORKSPACE_TOOLS.into_iter().map(str::to_string));
    }
    if task.agent_type.as_deref() == Some("computer") {
        names.push(BASH_TOOL_NAME.to_string());
        names.push(CHECK_BACKGROUND_COMMAND_TOOL_NAME.to_string());
    }
    if task.sub_agents_enabled() {
        names.push(CREATE_SUB_TASK_TOOL_NAME.to_string());
        names.push(SUB_TASK_STATUS_TOOL_NAME.to_string());
    }
    if task
        .metadata
        .get("available_skills")
        .is_some_and(|value| !value.is_null())
    {
        names.push(ACTIVATE_SKILL_TOOL_NAME.to_string());
    }
    if task.native_multimodal {
        names.push(READ_IMAGE_TOOL_NAME.to_string());
    }
    names.extend(task.extra_tool_names.clone());
    if !task.exclude_tools.is_empty() {
        names.retain(|name| !task.exclude_tools.contains(name));
    }

    let mut deduped = Vec::new();
    for name in names {
        if !deduped.contains(&name) {
            deduped.push(name);
        }
    }
    deduped
}

pub fn plan_tool_schemas(
    registry: &ToolRegistry,
    task: &AgentTask,
    memory_usage_percentage: Option<u32>,
) -> Vec<Value> {
    let names = plan_tool_names(task, memory_usage_percentage);
    let available_names = names
        .into_iter()
        .filter(|name| registry.has_tool(name) && registry.has_schema(name))
        .collect::<Vec<_>>();
    let schemas = registry
        .list_openai_schemas(Some(&available_names))
        .expect("planned tool names were pre-filtered to registered schemas");
    patch_dynamic_tool_schema_hints(task, schemas)
}

pub fn freeze_dynamic_tool_schema_hints(task: &mut AgentTask) {
    if task.agent_type.as_deref() == Some("computer")
        || task.extra_tool_names.iter().any(|name| name == "bash")
    {
        let hint = build_bash_runtime_hint(task);
        task.metadata.insert(
            BASH_RUNTIME_HINT_METADATA_KEY.to_string(),
            Value::String(hint),
        );
    }
}

pub fn patch_dynamic_tool_schema_hints(task: &AgentTask, tool_schemas: Vec<Value>) -> Vec<Value> {
    let mut bash_hint = None::<String>;
    tool_schemas
        .into_iter()
        .map(|mut schema| {
            if schema["function"]["name"].as_str() != Some("bash") {
                return schema;
            }
            let hint = bash_hint.get_or_insert_with(|| build_bash_runtime_hint(task));
            let base_description = schema["function"]["description"]
                .as_str()
                .unwrap_or_default()
                .trim_end()
                .to_string();
            schema["function"]["description"] =
                Value::String(format!("{base_description}\n\n{hint}").trim().to_string());
            schema
        })
        .collect()
}

fn build_bash_runtime_hint(task: &AgentTask) -> String {
    if let Some(cached) = task
        .metadata
        .get(BASH_RUNTIME_HINT_METADATA_KEY)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return cached.to_string();
    }
    let shell = task
        .metadata
        .get("bash_shell")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let windows_shell_priority =
        match normalize_windows_shell_priority(task.metadata.get("windows_shell_priority")) {
            Ok(priority) => priority,
            Err(error) => return format!("Runtime shell hint: invalid shell config. {error}."),
        };
    match resolve_shell_invocation(shell, windows_shell_priority.as_deref()) {
        Ok(resolved) => format!(
            "Runtime shell hint: commands run via `{}` using prefix `{}`.",
            resolved.kind,
            resolved.prefix.join(" ")
        ),
        Err(error) => format!("Runtime shell hint: invalid shell config. {error}."),
    }
}
