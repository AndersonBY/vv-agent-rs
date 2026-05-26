use serde_json::Value;

use crate::types::AgentTask;

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
    let resolved = resolve_shell_invocation(shell, windows_shell_priority.as_deref());
    format!(
        "Runtime shell hint: commands run via `{}` using prefix `{}`.",
        resolved.kind,
        resolved.prefix.join(" ")
    )
}

fn normalize_windows_shell_priority(raw: Option<&Value>) -> Result<Option<Vec<String>>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let Some(items) = raw.as_array() else {
        return Err("`windows_shell_priority` must be a list of shell names".to_string());
    };
    let mut normalized = Vec::new();
    for item in items {
        let value = item.as_str().unwrap_or_default().trim();
        if value.is_empty() || normalized.iter().any(|seen| seen == value) {
            continue;
        }
        normalized.push(value.to_string());
    }
    Ok(Some(normalized))
}

struct ResolvedShellInvocation {
    kind: String,
    prefix: Vec<String>,
}

fn resolve_shell_invocation(
    shell: Option<&str>,
    windows_shell_priority: Option<&[String]>,
) -> ResolvedShellInvocation {
    if cfg!(target_os = "windows") {
        let selected = shell
            .map(str::to_string)
            .or_else(|| {
                windows_shell_priority
                    .and_then(|priority| priority.first())
                    .cloned()
            })
            .unwrap_or_else(|| "cmd".to_string());
        return match selected.as_str() {
            "powershell" | "pwsh" => ResolvedShellInvocation {
                kind: selected.clone(),
                prefix: vec![
                    selected,
                    "-NoLogo".to_string(),
                    "-NoProfile".to_string(),
                    "-Command".to_string(),
                ],
            },
            other => ResolvedShellInvocation {
                kind: other.to_string(),
                prefix: vec![other.to_string(), "/C".to_string()],
            },
        };
    }
    let selected = shell.unwrap_or("bash");
    let normalized = selected.trim().to_ascii_lowercase().replace('_', "-");
    if normalized == "powershell" || normalized == "powershell.exe" || normalized == "pwsh" {
        return ResolvedShellInvocation {
            kind: selected.to_string(),
            prefix: vec![
                selected.to_string(),
                "-NoLogo".to_string(),
                "-NoProfile".to_string(),
                "-Command".to_string(),
            ],
        };
    }
    if normalized == "cmd" || normalized == "cmd.exe" {
        return ResolvedShellInvocation {
            kind: "cmd".to_string(),
            prefix: vec![selected.to_string(), "/C".to_string()],
        };
    }
    ResolvedShellInvocation {
        kind: "bash".to_string(),
        prefix: vec![selected.to_string(), "-lc".to_string()],
    }
}
