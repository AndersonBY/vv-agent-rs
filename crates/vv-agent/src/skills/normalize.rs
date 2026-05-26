use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::models::SkillEntry;
use super::parser::{discover_skill_dirs, find_skill_md, read_properties, read_skill};

pub fn normalize_skill_list(
    raw_skills: Option<&Value>,
    workspace: Option<&Path>,
    load_instructions: bool,
) -> Vec<SkillEntry> {
    let Some(Value::Array(raw_skills)) = raw_skills else {
        return Vec::new();
    };

    let mut entries = Vec::new();
    for item in raw_skills {
        match item {
            Value::String(path) => {
                entries.extend(entries_from_path(path.trim(), workspace, load_instructions));
            }
            Value::Object(object) => {
                entries.extend(entries_from_object(object, workspace, load_instructions));
            }
            _ => {}
        }
    }

    let mut deduped = Vec::new();
    for entry in entries {
        if !deduped
            .iter()
            .any(|known: &SkillEntry| known.name == entry.name)
        {
            deduped.push(entry);
        }
    }
    deduped
}

fn entries_from_path(
    raw_path: &str,
    workspace: Option<&Path>,
    load_instructions: bool,
) -> Vec<SkillEntry> {
    if raw_path.is_empty() || !path_exists(raw_path, workspace) {
        return Vec::new();
    }
    let resolved = resolve_skill_path(raw_path, workspace);
    if resolved.is_dir() && find_skill_md(&resolved).is_none() {
        return discover_skill_dirs(&resolved)
            .into_iter()
            .map(|skill_dir| load_entry(&skill_dir, workspace, load_instructions))
            .collect();
    }
    vec![load_entry(&resolved, workspace, load_instructions)]
}

fn entries_from_object(
    object: &serde_json::Map<String, Value>,
    workspace: Option<&Path>,
    load_instructions: bool,
) -> Vec<SkillEntry> {
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    let description = object
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    let location = object
        .get("location")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());

    if (name.is_empty() || description.is_empty())
        && location.is_some_and(|location| path_exists(location, workspace))
    {
        return entries_from_path(location.unwrap_or_default(), workspace, load_instructions);
    }

    if name.is_empty() || description.is_empty() {
        return Vec::new();
    }

    let instructions = object
        .get("instructions")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if instructions.is_none()
        && load_instructions
        && location.is_some_and(|location| path_exists(location, workspace))
    {
        let loaded = entries_from_path(location.unwrap_or_default(), workspace, true);
        if !loaded.is_empty() {
            return loaded;
        }
    }

    vec![SkillEntry {
        name: name.to_string(),
        description: description.to_string(),
        location: location.map(str::to_string),
        instructions,
        compatibility: object
            .get("compatibility")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        allowed_tools: object
            .get("allowed-tools")
            .or_else(|| object.get("allowed_tools"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        metadata: object
            .get("metadata")
            .and_then(Value::as_object)
            .map(string_map_from_json_object)
            .unwrap_or_default(),
        load_error: None,
    }]
}

fn load_entry(skill_dir: &Path, workspace: Option<&Path>, load_instructions: bool) -> SkillEntry {
    if load_instructions {
        match read_skill(skill_dir, Some("strict")) {
            Ok(loaded) => {
                return SkillEntry {
                    name: loaded.properties.name,
                    description: loaded.properties.description,
                    location: Some(relative_location(&loaded.skill_md_path, workspace)),
                    instructions: Some(loaded.instructions).filter(|value| !value.is_empty()),
                    compatibility: loaded.properties.compatibility,
                    allowed_tools: loaded.properties.allowed_tools,
                    metadata: loaded.properties.metadata,
                    load_error: None,
                };
            }
            Err(error) => return load_error_entry(skill_dir, error.to_string()),
        }
    }

    match read_properties(skill_dir) {
        Ok(properties) => {
            let location =
                find_skill_md(skill_dir).map(|skill_md| relative_location(&skill_md, workspace));
            SkillEntry {
                name: properties.name,
                description: properties.description,
                location,
                instructions: None,
                compatibility: properties.compatibility,
                allowed_tools: properties.allowed_tools,
                metadata: properties.metadata,
                load_error: None,
            }
        }
        Err(error) => load_error_entry(skill_dir, error.to_string()),
    }
}

fn load_error_entry(skill_dir: &Path, error: String) -> SkillEntry {
    SkillEntry {
        name: skill_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("skill")
            .to_string(),
        description: String::new(),
        location: Some(skill_dir.to_string_lossy().to_string()),
        load_error: Some(error),
        ..SkillEntry::default()
    }
}

fn path_exists(raw_path: &str, workspace: Option<&Path>) -> bool {
    let path = expand_user(raw_path);
    if path.exists() {
        return true;
    }
    if let Some(workspace) = workspace {
        if !path.is_absolute() {
            return workspace.join(&path).exists();
        }
    }
    false
}

fn resolve_skill_path(raw_path: &str, workspace: Option<&Path>) -> PathBuf {
    let path = expand_user(raw_path);
    let path = if path.is_absolute() {
        path
    } else if let Some(workspace) = workspace {
        workspace.join(path)
    } else {
        path
    };
    let path = path.canonicalize().unwrap_or(path);
    if path.is_file()
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("skill.md"))
    {
        return path.parent().map(Path::to_path_buf).unwrap_or(path);
    }
    path
}

fn relative_location(skill_md: &Path, workspace: Option<&Path>) -> String {
    let normalized_skill_md = skill_md
        .canonicalize()
        .unwrap_or_else(|_| skill_md.to_path_buf());
    if let Some(workspace) = workspace {
        let normalized_workspace = workspace
            .canonicalize()
            .unwrap_or_else(|_| workspace.to_path_buf());
        if let Ok(relative) = normalized_skill_md.strip_prefix(normalized_workspace) {
            return relative.to_string_lossy().replace('\\', "/");
        }
    }
    normalized_skill_md.to_string_lossy().replace('\\', "/")
}

fn expand_user(raw_path: &str) -> PathBuf {
    if let Some(rest) = raw_path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(raw_path)
}

fn string_map_from_json_object(
    object: &serde_json::Map<String, Value>,
) -> BTreeMap<String, String> {
    object
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                value
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| value.to_string()),
            )
        })
        .collect()
}
