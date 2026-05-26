use serde_json::Value;

use crate::types::AgentTask;

use super::shell::{normalize_windows_shell_priority, resolve_shell_invocation};

const BASH_RUNTIME_HINT_METADATA_KEY: &str = "_vv_agent_bash_runtime_hint";

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
