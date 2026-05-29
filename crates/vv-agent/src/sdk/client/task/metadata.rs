use serde_json::Value;

use crate::sdk::types::AgentDefinition;
use crate::types::{AgentTask, Metadata};

pub(in crate::sdk::client) fn metadata_from_definition(definition: &AgentDefinition) -> Metadata {
    let mut metadata = definition.metadata.clone();
    metadata
        .entry("language".to_string())
        .or_insert_with(|| Value::String(definition.language.clone()));
    if let Some(shell) = definition.bash_shell.as_ref() {
        metadata
            .entry("bash_shell".to_string())
            .or_insert_with(|| Value::String(shell.clone()));
    }
    if !definition.windows_shell_priority.is_empty() {
        metadata
            .entry("windows_shell_priority".to_string())
            .or_insert_with(|| {
                Value::Array(
                    definition
                        .windows_shell_priority
                        .iter()
                        .cloned()
                        .map(Value::String)
                        .collect(),
                )
            });
    }
    if !definition.bash_env.is_empty() {
        metadata
            .entry("bash_env".to_string())
            .or_insert_with(|| serde_json::to_value(&definition.bash_env).unwrap_or(Value::Null));
    }
    if !definition.sub_agents.is_empty() {
        metadata
            .entry("sub_agent_names".to_string())
            .or_insert_with(|| {
                Value::Array(
                    definition
                        .sub_agents
                        .keys()
                        .cloned()
                        .map(Value::String)
                        .collect(),
                )
            });
    }
    if !definition.skill_directories.is_empty() {
        metadata
            .entry("available_skills".to_string())
            .or_insert_with(|| {
                Value::Array(
                    definition
                        .skill_directories
                        .iter()
                        .cloned()
                        .map(Value::String)
                        .collect(),
                )
            });
    }
    metadata
}

pub(in crate::sdk::client) fn normalize_prepare_session_id(
    session_id: Option<String>,
) -> Option<String> {
    session_id
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(in crate::sdk::client) fn merge_request_metadata(task: &mut AgentTask, metadata: Metadata) {
    for (key, value) in metadata {
        task.metadata.entry(key).or_insert(value);
    }
}
