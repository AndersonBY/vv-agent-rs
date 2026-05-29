use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::sdk::types::AgentDefinition;

use super::models::DiscoveredResources;
use super::parse::{
    read_bool, read_metadata, read_no_tool_policy, read_percentage_u8, read_positive_u32,
    read_positive_u64, read_string, read_string_list, read_string_map, read_sub_agents,
};
use super::paths::{read_resolved_path_list, resolve_existing_or_absolute_path};

#[derive(Debug, Clone)]
pub struct AgentResourceLoader {
    pub workspace: PathBuf,
    pub project_resource_dir: PathBuf,
    pub global_resource_dir: PathBuf,
    cached: Option<DiscoveredResources>,
}

impl AgentResourceLoader {
    pub fn new(workspace: impl Into<PathBuf>) -> Self {
        let workspace = resolve_existing_or_absolute_path(workspace.into());
        Self {
            project_resource_dir: resolve_existing_or_absolute_path(workspace.join(".vv-agent")),
            global_resource_dir: resolve_existing_or_absolute_path(PathBuf::from("~/.vv-agent")),
            workspace,
            cached: None,
        }
    }

    pub fn with_resource_dirs(
        workspace: impl Into<PathBuf>,
        project_resource_dir: impl Into<PathBuf>,
        global_resource_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            workspace: resolve_existing_or_absolute_path(workspace.into()),
            project_resource_dir: resolve_existing_or_absolute_path(project_resource_dir.into()),
            global_resource_dir: resolve_existing_or_absolute_path(global_resource_dir.into()),
            cached: None,
        }
    }

    pub fn discover(&mut self) -> DiscoveredResources {
        self.discover_inner(false)
    }

    pub fn discover_force_reload(&mut self) -> DiscoveredResources {
        self.discover_inner(true)
    }

    fn discover_inner(&mut self, force_reload: bool) -> DiscoveredResources {
        if let Some(cached) = &self.cached {
            if !force_reload {
                return cached.clone();
            }
        }
        let mut discovered = DiscoveredResources::default();
        for root in [
            self.global_resource_dir.clone(),
            self.project_resource_dir.clone(),
        ] {
            if root.is_dir() {
                self.load_agents(&root, &mut discovered);
                self.load_prompts(&root, &mut discovered);
                self.load_skills(&root, &mut discovered);
            }
        }
        self.cached = Some(discovered.clone());
        discovered
    }

    fn load_agents(&self, root: &Path, discovered: &mut DiscoveredResources) {
        let config_file = root.join("agents.json");
        if !config_file.is_file() {
            return;
        }
        let raw = match std::fs::read_to_string(&config_file)
            .ok()
            .and_then(|content| serde_json::from_str::<Value>(&content).ok())
        {
            Some(raw) => raw,
            None => {
                discovered
                    .diagnostics
                    .push(format!("Invalid agents.json in {}", root.display()));
                return;
            }
        };
        let profiles = raw
            .get("profiles")
            .and_then(Value::as_object)
            .or_else(|| raw.as_object());
        let Some(profiles) = profiles else {
            discovered.diagnostics.push(format!(
                "agents.json in {} must be an object or contain `profiles` object.",
                root.display()
            ));
            return;
        };
        for (name, payload) in profiles {
            if name.trim().is_empty() {
                discovered.diagnostics.push(format!(
                    "Skip invalid profile name in {}.",
                    config_file.display()
                ));
                continue;
            }
            let Some(payload) = payload.as_object() else {
                discovered.diagnostics.push(format!(
                    "Skip profile `{name}` in {}: definition must be an object.",
                    config_file.display()
                ));
                continue;
            };
            let Some(description) = read_string(payload, "description") else {
                discovered.diagnostics.push(format!(
                    "Skip profile `{name}`: `description` must be non-empty string."
                ));
                continue;
            };
            let Some(model) = read_string(payload, "model") else {
                discovered.diagnostics.push(format!(
                    "Skip profile `{name}`: `model` must be non-empty string."
                ));
                continue;
            };
            let mut definition = AgentDefinition::default_for_model(model);
            definition.description = description;
            definition.backend = read_string(payload, "backend");
            definition.language =
                read_string(payload, "language").unwrap_or_else(|| "zh-CN".to_string());
            definition.max_cycles = read_positive_u32(payload, "max_cycles", 10);
            definition.memory_compact_threshold =
                read_positive_u64(payload, "memory_compact_threshold", 128_000);
            definition.memory_threshold_percentage =
                read_percentage_u8(payload, "memory_threshold_percentage", 90);
            definition.no_tool_policy = read_no_tool_policy(payload);
            definition.allow_interruption = read_bool(payload, "allow_interruption", true);
            definition.use_workspace = read_bool(payload, "use_workspace", true);
            definition.enable_todo_management = read_bool(payload, "enable_todo_management", true);
            definition.agent_type = read_string(payload, "agent_type");
            definition.native_multimodal = read_bool(payload, "native_multimodal", false);
            definition.enable_sub_agents = read_bool(payload, "enable_sub_agents", false);
            definition.sub_agents = read_sub_agents(payload);
            definition.extra_tool_names = read_string_list(payload, "extra_tool_names");
            definition.exclude_tools = read_string_list(payload, "exclude_tools");
            definition.bash_shell = read_string(payload, "bash_shell");
            definition.windows_shell_priority = read_string_list(payload, "windows_shell_priority");
            definition.bash_env = read_string_map(payload, "bash_env");
            definition.metadata = read_metadata(payload, "metadata");
            definition.system_prompt = read_string(payload, "system_prompt");
            definition.system_prompt_template = read_string(payload, "system_prompt_template");
            definition.skill_directories =
                read_resolved_path_list(payload, "skill_directories", root);
            discovered.agents.insert(name.clone(), definition);
        }
    }

    fn load_prompts(&self, root: &Path, discovered: &mut DiscoveredResources) {
        let prompts_dir = root.join("prompts");
        if !prompts_dir.is_dir() {
            return;
        }
        let Ok(entries) = std::fs::read_dir(prompts_dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
                continue;
            }
            if let (Some(stem), Ok(content)) = (
                path.file_stem().and_then(|stem| stem.to_str()),
                std::fs::read_to_string(&path),
            ) {
                discovered.prompts.insert(stem.to_string(), content);
            }
        }
    }

    fn load_skills(&self, root: &Path, discovered: &mut DiscoveredResources) {
        let skills_dir = root.join("skills");
        if !skills_dir.is_dir() {
            return;
        }
        let skills_dir = skills_dir.canonicalize().unwrap_or(skills_dir);
        let path = skills_dir.to_string_lossy().to_string();
        if !discovered.skill_directories.contains(&path) {
            discovered.skill_directories.push(path);
        }
    }
}
