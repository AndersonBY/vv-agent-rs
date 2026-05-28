use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::Value;

use crate::types::{Metadata, NoToolPolicy, SubAgentConfig};

use super::types::AgentDefinition;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DiscoveredResources {
    pub agents: BTreeMap<String, AgentDefinition>,
    pub prompts: BTreeMap<String, String>,
    pub skill_directories: Vec<String>,
    pub hooks: Vec<String>,
    pub hook_files: Vec<String>,
    pub diagnostics: Vec<String>,
}

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
                self.load_hooks(&root, &mut discovered);
            }
        }
        self.cached = Some(discovered.clone());
        discovered
    }

    fn load_agents(&self, root: &std::path::Path, discovered: &mut DiscoveredResources) {
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
            let Some(description) = payload
                .get("description")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                discovered.diagnostics.push(format!(
                    "Skip profile `{name}`: `description` must be non-empty string."
                ));
                continue;
            };
            let Some(model) = payload
                .get("model")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                discovered.diagnostics.push(format!(
                    "Skip profile `{name}`: `model` must be non-empty string."
                ));
                continue;
            };
            let mut definition = AgentDefinition::default_for_model(model);
            definition.description = description.to_string();
            definition.backend = read_string(payload, "backend");
            definition.language =
                read_string(payload, "language").unwrap_or_else(|| "zh-CN".to_string());
            definition.max_cycles = read_u32(payload, "max_cycles", 10).max(1);
            definition.memory_compact_threshold =
                read_u64(payload, "memory_compact_threshold", 128_000);
            definition.memory_threshold_percentage =
                read_u8(payload, "memory_threshold_percentage", 90);
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

    fn load_prompts(&self, root: &std::path::Path, discovered: &mut DiscoveredResources) {
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

    fn load_skills(&self, root: &std::path::Path, discovered: &mut DiscoveredResources) {
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

    fn load_hooks(&self, root: &std::path::Path, discovered: &mut DiscoveredResources) {
        let hooks_dir = root.join("hooks");
        if !hooks_dir.is_dir() {
            return;
        }

        let mut hook_files = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&hooks_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("py") {
                    hook_files.push(path);
                } else if path.is_dir() {
                    let index_file = path.join("index.py");
                    if index_file.is_file() {
                        hook_files.push(index_file);
                    }
                }
            }
        }
        hook_files.sort();

        for hook_file in hook_files {
            let hook_file = hook_file.canonicalize().unwrap_or(hook_file);
            let path = hook_file.to_string_lossy().to_string();
            if !discovered.hook_files.contains(&path) {
                discovered.hook_files.push(path.clone());
            }
            if !discovered.hooks.contains(&path) {
                discovered.hooks.push(path.clone());
            }
        }
    }
}

fn read_string(payload: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn read_bool(payload: &serde_json::Map<String, Value>, key: &str, default: bool) -> bool {
    payload.get(key).map(python_truthy).unwrap_or(default)
}

fn python_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(number) => number.as_f64() != Some(0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

fn read_u32(payload: &serde_json::Map<String, Value>, key: &str, default: u32) -> u32 {
    payload
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(default)
}

fn read_u64(payload: &serde_json::Map<String, Value>, key: &str, default: u64) -> u64 {
    payload.get(key).and_then(Value::as_u64).unwrap_or(default)
}

fn read_u8(payload: &serde_json::Map<String, Value>, key: &str, default: u8) -> u8 {
    payload
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u8::try_from(value).ok())
        .unwrap_or(default)
}

fn read_no_tool_policy(payload: &serde_json::Map<String, Value>) -> NoToolPolicy {
    match read_string(payload, "no_tool_policy").as_deref() {
        Some("finish") => NoToolPolicy::Finish,
        Some("wait_user") => NoToolPolicy::WaitUser,
        _ => NoToolPolicy::Continue,
    }
}

fn read_string_list(payload: &serde_json::Map<String, Value>, key: &str) -> Vec<String> {
    payload
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(python_str)
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn read_string_map(
    payload: &serde_json::Map<String, Value>,
    key: &str,
) -> BTreeMap<String, String> {
    payload
        .get(key)
        .and_then(Value::as_object)
        .map(|object| {
            object
                .iter()
                .filter_map(|(key, value)| {
                    let key = key.trim();
                    if key.is_empty() {
                        return None;
                    }
                    Some((key.to_string(), python_str(value)))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn python_str(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(value) => value.clone(),
        Value::Array(items) => {
            let items = items.iter().map(python_repr).collect::<Vec<_>>().join(", ");
            format!("[{items}]")
        }
        Value::Object(object) => {
            let items = object
                .iter()
                .map(|(key, value)| format!("{}: {}", python_repr_string(key), python_repr(value)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{{items}}}")
        }
    }
}

fn python_repr(value: &Value) -> String {
    match value {
        Value::String(value) => python_repr_string(value),
        other => python_str(other),
    }
}

fn python_repr_string(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn read_metadata(payload: &serde_json::Map<String, Value>, key: &str) -> Metadata {
    payload
        .get(key)
        .and_then(Value::as_object)
        .map(|object| {
            object
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default()
}

fn read_resolved_path_list(
    payload: &serde_json::Map<String, Value>,
    key: &str,
    base_dir: &std::path::Path,
) -> Vec<String> {
    payload
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|path| resolve_resource_path(base_dir, path))
                .collect()
        })
        .unwrap_or_default()
}

fn resolve_resource_path(base_dir: &std::path::Path, raw_path: &str) -> String {
    let path = expand_user_path(PathBuf::from(raw_path));
    let path = if path.is_absolute() {
        path
    } else {
        absolutize_path(base_dir.join(path))
    };
    let path = path.canonicalize().unwrap_or(path);
    path.to_string_lossy().to_string()
}

fn resolve_existing_or_absolute_path(path: PathBuf) -> PathBuf {
    let path = absolutize_path(expand_user_path(path));
    path.canonicalize().unwrap_or(path)
}

fn expand_user_path(path: PathBuf) -> PathBuf {
    let raw_path = path.to_string_lossy();
    if raw_path == "~" {
        return home_dir().unwrap_or(path);
    }
    if let Some(rest) = raw_path.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    }
    path
}

fn absolutize_path(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        return path;
    }
    std::env::current_dir()
        .map(|current_dir| current_dir.join(&path))
        .unwrap_or(path)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .or_else(|| {
            let drive = std::env::var_os("HOMEDRIVE")?;
            let path = std::env::var_os("HOMEPATH")?;
            let mut home = PathBuf::from(drive);
            home.push(path);
            Some(home)
        })
}

fn read_sub_agents(payload: &serde_json::Map<String, Value>) -> BTreeMap<String, SubAgentConfig> {
    let mut sub_agents = BTreeMap::new();
    let Some(object) = payload.get("sub_agents").and_then(Value::as_object) else {
        return sub_agents;
    };
    for (name, raw_config) in object {
        let Some(config) = raw_config.as_object() else {
            continue;
        };
        let Some(model) = read_string(config, "model") else {
            continue;
        };
        let Some(description) = read_string(config, "description") else {
            continue;
        };
        let mut sub_agent = SubAgentConfig::new(model, description);
        sub_agent.backend = read_string(config, "backend");
        sub_agent.system_prompt = read_string(config, "system_prompt");
        sub_agent.max_cycles = read_u32(config, "max_cycles", 8).max(1);
        sub_agent.exclude_tools = read_string_list(config, "exclude_tools");
        sub_agent.metadata = read_metadata(config, "metadata");
        sub_agents.insert(name.clone(), sub_agent);
    }
    sub_agents
}
