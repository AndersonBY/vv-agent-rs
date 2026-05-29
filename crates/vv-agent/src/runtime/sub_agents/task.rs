use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use crate::prompt::{
    build_raw_system_prompt_sections, build_system_prompt_bundle_with_options,
    BuildSystemPromptOptions,
};
use crate::types::{AgentTask, NoToolPolicy, SubAgentConfig, SubTaskRequest};

use super::types::{SubTaskBuildInputs, SubTaskRunContext};

pub(super) fn build_sub_agent_task(
    context: &SubTaskRunContext,
    inputs: SubTaskBuildInputs<'_>,
) -> AgentTask {
    let parent_task = &context.parent_task;
    let sub_agent = inputs.sub_agent;
    let request = inputs.request;
    let (system_prompt, generated_sections) = if let Some(system_prompt) = &sub_agent.system_prompt
    {
        (
            system_prompt.clone(),
            build_raw_system_prompt_sections(system_prompt),
        )
    } else {
        let language = parent_task
            .metadata
            .get("language")
            .and_then(Value::as_str)
            .unwrap_or("zh-CN")
            .to_string();
        let available_skills = parent_task
            .metadata
            .get("available_skills")
            .filter(|value| value.is_array())
            .cloned();
        let prompt_bundle = build_system_prompt_bundle_with_options(
            &sub_agent.description,
            BuildSystemPromptOptions {
                language,
                allow_interruption: false,
                use_workspace: parent_task.use_workspace,
                enable_todo_management: true,
                agent_type: parent_task.agent_type.clone(),
                available_skills,
                workspace: Some(context.workspace_path.clone()),
                ..BuildSystemPromptOptions::default()
            },
        );
        (prompt_bundle.prompt, prompt_bundle.sections)
    };
    let mut user_prompt = request.task_description.clone();
    if !request.output_requirements.is_empty() {
        user_prompt.push_str("\n\n<Output Requirements>\n");
        user_prompt.push_str(&request.output_requirements);
        user_prompt.push_str("\n</Output Requirements>");
    }
    if request.include_main_summary {
        let parent_summary = build_parent_summary(parent_task, &context.parent_shared_state);
        if !parent_summary.is_empty() {
            user_prompt.push_str("\n\n<Main Task Summary>\n");
            user_prompt.push_str(&parent_summary);
            user_prompt.push_str("\n</Main Task Summary>");
        }
    }

    let mut sub_task = AgentTask::new(
        inputs.sub_task_id,
        inputs.resolved_model_id.to_string(),
        system_prompt,
        user_prompt,
    );
    sub_task.max_cycles = sub_agent.max_cycles.max(1);
    sub_task.memory_compact_threshold = parent_task.memory_compact_threshold;
    sub_task.memory_threshold_percentage = parent_task.memory_threshold_percentage;
    sub_task.no_tool_policy = NoToolPolicy::Continue;
    sub_task.allow_interruption = false;
    sub_task.use_workspace = parent_task.use_workspace;
    sub_task.has_sub_agents = false;
    sub_task.sub_agents = BTreeMap::new();
    sub_task.agent_type = parent_task.agent_type.clone();
    sub_task.native_multimodal = parent_task.native_multimodal;
    sub_task.extra_tool_names = parent_task.extra_tool_names.clone();
    sub_task.exclude_tools = merged_sub_task_exclusions(parent_task, sub_agent);
    sub_task.metadata = build_sub_task_metadata(
        parent_task,
        inputs.sub_task_id,
        inputs.sub_session_id,
        inputs.sub_agent_name,
        request,
        &context.workspace_path,
        generated_sections,
    );
    sub_task
}

fn merged_sub_task_exclusions(parent_task: &AgentTask, sub_agent: &SubAgentConfig) -> Vec<String> {
    let mut excluded = parent_task.exclude_tools.clone();
    excluded.extend(sub_agent.exclude_tools.clone());
    excluded.push(crate::constants::CREATE_SUB_TASK_TOOL_NAME.to_string());
    excluded.push(crate::constants::SUB_TASK_STATUS_TOOL_NAME.to_string());
    excluded.sort();
    excluded.dedup();
    excluded
}

fn build_sub_task_metadata(
    parent_task: &AgentTask,
    sub_task_id: &str,
    sub_session_id: &str,
    sub_agent_name: &str,
    request: &SubTaskRequest,
    workspace_path: &Path,
    system_prompt_sections: Vec<Value>,
) -> BTreeMap<String, Value> {
    let mut metadata = BTreeMap::from([
        ("is_sub_task".to_string(), Value::Bool(true)),
        (
            "parent_task_id".to_string(),
            Value::String(parent_task.task_id.clone()),
        ),
        (
            "sub_agent_name".to_string(),
            Value::String(sub_agent_name.to_string()),
        ),
        ("session_memory_enabled".to_string(), Value::Bool(false)),
        (
            "workspace".to_string(),
            Value::String(workspace_path.display().to_string()),
        ),
    ]);
    for key in [
        "bash_shell",
        "windows_shell_priority",
        "bash_env",
        "allow_outside_workspace_paths",
        "allow_outside_workspace",
        "workspace_allow_outside_main",
        "workspace_allow_outside",
        "language",
        "available_skills",
        "active_skills",
    ] {
        if let Some(value) = parent_task.metadata.get(key) {
            metadata.insert(key.to_string(), value.clone());
        }
    }
    if let Some(sub_agent) = parent_task.sub_agents.get(sub_agent_name) {
        metadata.extend(sub_agent.metadata.clone());
    }
    metadata.extend(request.metadata.clone());
    if !system_prompt_sections.is_empty() {
        metadata
            .entry("system_prompt_sections".to_string())
            .or_insert(Value::Array(system_prompt_sections));
    }
    metadata.insert(
        "task_id".to_string(),
        Value::String(sub_task_id.to_string()),
    );
    metadata.insert(
        "session_id".to_string(),
        Value::String(sub_session_id.to_string()),
    );
    metadata.insert(
        "browser_scope_key".to_string(),
        Value::String(sub_session_id.to_string()),
    );
    metadata
}

fn build_parent_summary(
    parent_task: &AgentTask,
    parent_shared_state: &BTreeMap<String, Value>,
) -> String {
    let mut lines = vec![format!("Parent task goal: {}", parent_task.user_prompt)];
    if let Some(todo_list) = parent_shared_state
        .get("todo_list")
        .and_then(Value::as_array)
    {
        if !todo_list.is_empty() {
            lines.push("Parent TODO status:".to_string());
            for item in todo_list {
                let title = item
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("Untitled");
                let status = item
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("pending");
                lines.push(format!("- [{status}] {title}"));
            }
        }
    }
    lines.join("\n")
}
