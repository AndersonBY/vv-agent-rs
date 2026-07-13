use std::collections::BTreeSet;

use serde_json::Value;

use crate::constants::{
    ACTIVATE_SKILL_TOOL_NAME, ASK_USER_TOOL_NAME, BASH_TOOL_NAME,
    CHECK_BACKGROUND_COMMAND_TOOL_NAME, CREATE_SUB_TASK_TOOL_NAME, READ_IMAGE_TOOL_NAME,
    SUB_TASK_STATUS_TOOL_NAME, TASK_FINISH_TOOL_NAME, WORKSPACE_TOOLS,
};
use crate::tools::{ToolPolicy, ToolRegistry};
use crate::types::AgentTask;

use super::shell::{normalize_windows_shell_priority, resolve_shell_invocation};

const BASH_RUNTIME_HINT_METADATA_KEY: &str = "_vv_agent_bash_runtime_hint";
const ALLOWED_TOOLS_METADATA_KEY: &str = "_vv_agent_allowed_tools";
const DISALLOWED_TOOLS_METADATA_KEY: &str = "_vv_agent_disallowed_tools";
const TOOL_POLICY_APPROVAL_METADATA_KEY: &str = "_vv_agent_tool_policy_approval";
const TOOL_POLICY_CAN_USE_TOOL_METADATA_KEY: &str = "_vv_agent_tool_policy_can_use_tool";

pub(crate) fn project_tool_policy(task: &mut AgentTask, policy: &ToolPolicy) {
    match policy.allowed_tools.as_ref() {
        Some(allowed_tools) => {
            task.metadata.insert(
                ALLOWED_TOOLS_METADATA_KEY.to_string(),
                Value::Array(allowed_tools.iter().cloned().map(Value::String).collect()),
            );
        }
        None => {
            task.metadata.remove(ALLOWED_TOOLS_METADATA_KEY);
        }
    }
    if policy.disallowed_tools.is_empty() {
        task.metadata.remove(DISALLOWED_TOOLS_METADATA_KEY);
    } else {
        task.metadata.insert(
            DISALLOWED_TOOLS_METADATA_KEY.to_string(),
            Value::Array(
                policy
                    .disallowed_tools
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        );
    }
    match policy.approval {
        crate::tools::ApprovalPolicy::Default => {
            task.metadata.remove(TOOL_POLICY_APPROVAL_METADATA_KEY);
        }
        approval => {
            let value = match approval {
                crate::tools::ApprovalPolicy::Never => "never",
                crate::tools::ApprovalPolicy::Always => "always",
                crate::tools::ApprovalPolicy::OnRequest => "on_request",
                crate::tools::ApprovalPolicy::Default => unreachable!(),
            };
            task.metadata.insert(
                TOOL_POLICY_APPROVAL_METADATA_KEY.to_string(),
                Value::String(value.to_string()),
            );
        }
    }
    if policy.can_use_tool.is_some() {
        task.metadata.insert(
            TOOL_POLICY_CAN_USE_TOOL_METADATA_KEY.to_string(),
            Value::Bool(true),
        );
    } else {
        task.metadata.remove(TOOL_POLICY_CAN_USE_TOOL_METADATA_KEY);
    }
}

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
        .is_some_and(is_json_truthy)
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
    if let Some(disallowed_tools) = metadata_tool_names(task, DISALLOWED_TOOLS_METADATA_KEY) {
        names.retain(|name| !disallowed_tools.contains(name.as_str()));
    }
    if let Some(allowed_tools) = metadata_tool_names(task, ALLOWED_TOOLS_METADATA_KEY) {
        names.retain(|name| allowed_tools.contains(name.as_str()));
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
    let shell = match task.metadata.get("bash_shell") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => {
            let value = value.trim();
            (!value.is_empty()).then_some(value)
        }
        Some(_) => {
            return invalid_shell_hint("`bash_shell` must be a string shell name");
        }
    };
    let windows_shell_priority =
        match normalize_windows_shell_priority(task.metadata.get("windows_shell_priority")) {
            Ok(priority) => priority,
            Err(error) => return invalid_shell_hint(error),
        };
    match resolve_shell_invocation(shell, windows_shell_priority.as_deref()) {
        Ok(resolved) => format!(
            "Runtime shell hint: commands run via `{}` using prefix `{}`.",
            resolved.kind,
            resolved.prefix.join(" ")
        ),
        Err(error) => invalid_shell_hint(error),
    }
}

fn invalid_shell_hint(error: impl std::fmt::Display) -> String {
    let message = error.to_string();
    let message = message.trim_end_matches('.');
    format!("Runtime shell hint: invalid shell config. {message}.")
}

fn is_json_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value
            .as_i64()
            .map(|number| number != 0)
            .or_else(|| value.as_u64().map(|number| number != 0))
            .or_else(|| value.as_f64().map(|number| number != 0.0))
            .unwrap_or(true),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

fn metadata_tool_names<'a>(task: &'a AgentTask, key: &str) -> Option<BTreeSet<&'a str>> {
    task.metadata
        .get(key)
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .collect::<BTreeSet<_>>()
        })
}
