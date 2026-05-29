use std::path::Path;

use serde_json::Value;

use crate::prompt::{
    build_raw_system_prompt_sections, build_system_prompt_bundle_with_options,
    BuildSystemPromptOptions,
};
use crate::sdk::types::AgentDefinition;
use crate::types::AgentTask;

use super::ids::generate_task_id;
use super::metadata::metadata_from_definition;

pub(in crate::sdk::client) fn task_from_definition_with_task_name(
    definition: &AgentDefinition,
    prompt: String,
    workspace: Option<&Path>,
    task_name: Option<&str>,
) -> AgentTask {
    let (system_prompt, system_prompt_sections) =
        system_prompt_from_definition(definition, workspace);
    let mut task = AgentTask::new(
        generate_task_id(task_name.unwrap_or("inline")),
        definition.model.clone(),
        system_prompt,
        prompt,
    );
    task.max_cycles = definition.max_cycles.max(1);
    task.memory_compact_threshold = definition.memory_compact_threshold.max(1);
    task.memory_threshold_percentage = definition.memory_threshold_percentage.clamp(1, 100);
    task.no_tool_policy = definition.no_tool_policy;
    task.allow_interruption = definition.allow_interruption;
    task.use_workspace = definition.use_workspace;
    task.has_sub_agents = definition.enable_sub_agents;
    task.sub_agents = definition.sub_agents.clone();
    task.agent_type = definition.agent_type.clone();
    task.native_multimodal = definition.native_multimodal;
    task.extra_tool_names = definition.extra_tool_names.clone();
    task.exclude_tools = definition.exclude_tools.clone();
    task.metadata = metadata_from_definition(definition);
    if !system_prompt_sections.is_empty() {
        task.metadata
            .entry("system_prompt_sections".to_string())
            .or_insert(Value::Array(system_prompt_sections));
    }
    task
}

fn system_prompt_from_definition(
    definition: &AgentDefinition,
    workspace: Option<&Path>,
) -> (String, Vec<Value>) {
    if let Some(system_prompt) = definition.system_prompt.as_ref() {
        return (
            system_prompt.clone(),
            build_raw_system_prompt_sections(system_prompt),
        );
    }

    let available_sub_agents = definition
        .sub_agents
        .iter()
        .map(|(name, config)| (name.clone(), config.description.clone()))
        .collect();
    let available_skills = definition
        .metadata
        .get("available_skills")
        .cloned()
        .or_else(|| {
            (!definition.skill_directories.is_empty()).then(|| {
                Value::Array(
                    definition
                        .skill_directories
                        .iter()
                        .cloned()
                        .map(Value::String)
                        .collect(),
                )
            })
        });
    let prompt_bundle = build_system_prompt_bundle_with_options(
        &definition.description,
        BuildSystemPromptOptions {
            language: definition.language.clone(),
            allow_interruption: definition.allow_interruption,
            use_workspace: definition.use_workspace,
            enable_todo_management: definition.enable_todo_management,
            agent_type: definition.agent_type.clone(),
            available_sub_agents,
            available_skills,
            workspace: workspace.map(Path::to_path_buf),
            ..BuildSystemPromptOptions::default()
        },
    );
    (prompt_bundle.prompt, prompt_bundle.sections)
}
