use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};

use crate::tools::base::ToolSpec;
use crate::tools::common::{tool_error_with_code, tool_result};
use crate::types::{ToolDirective, ToolResultStatus};

pub(crate) fn activate_skill_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "activate_skill",
        "Activate a skill from the current task's available skill list.",
        Arc::new(|context, arguments| {
            let skill_name = arguments
                .get("skill_name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim();
            let reason = arguments
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            if skill_name.is_empty() {
                return tool_error_with_code("`skill_name` is required", "skill_name_required");
            }
            let raw_skills = context
                .shared_state
                .get("available_skills")
                .or_else(|| context.metadata.get("available_skills"));
            let entries = normalize_skill_list(raw_skills, &context.workspace, true);
            if entries.is_empty() {
                return tool_error_with_code(
                    "No skills are configured for this task",
                    "no_skills_configured",
                );
            }
            let Some(entry) = entries.into_iter().find(|entry| entry.name == skill_name) else {
                return tool_error_with_code(
                    format!("Skill '{skill_name}' is not allowed for this task"),
                    "skill_not_allowed",
                );
            };
            if let Some(error) = entry.load_error {
                return tool_error_with_code(
                    format!("Skill '{skill_name}' is invalid: {error}"),
                    "skill_invalid",
                );
            }

            let instructions = entry.instructions.filter(|text| !text.is_empty()).unwrap_or_else(|| {
                format!("Skill '{skill_name}' is activated, but no instruction text is available. Please inspect the skill files or provide explicit instructions.")
            });
            append_unique_string(
                &mut context.shared_state,
                "active_skills",
                entry.name.clone(),
            );
            append_activation_log(
                &mut context.shared_state,
                entry.name.clone(),
                reason.clone(),
                context.cycle_index,
            );
            let mut payload = serde_json::Map::from_iter([
                ("status".to_string(), Value::String("activated".to_string())),
                ("skill_name".to_string(), Value::String(entry.name.clone())),
                (
                    "message".to_string(),
                    Value::String(format!(
                        "Skill '{}' has been activated. Follow the instructions below.",
                        entry.name
                    )),
                ),
                ("instructions".to_string(), Value::String(instructions)),
            ]);
            if !entry.description.is_empty() {
                payload.insert("description".to_string(), Value::String(entry.description));
            }
            if let Some(location) = entry.location {
                payload.insert("location".to_string(), Value::String(location));
            }
            if let Some(compatibility) = entry.compatibility {
                payload.insert("compatibility".to_string(), Value::String(compatibility));
            }
            if let Some(allowed_tools) = entry.allowed_tools {
                payload.insert("allowed_tools".to_string(), Value::String(allowed_tools));
            }
            if !entry.metadata.is_empty() {
                payload.insert("metadata".to_string(), json!(entry.metadata));
            }
            if !reason.is_empty() {
                payload.insert("reason".to_string(), Value::String(reason));
            }
            tool_result(
                ToolResultStatus::Success,
                Value::Object(payload),
                None,
                ToolDirective::Continue,
            )
        }),
    );
    if let Some(schema) = super::super::schemas::schema_for("activate_skill") {
        spec.schema = schema;
    }
    spec
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct SkillEntry {
    name: String,
    description: String,
    location: Option<String>,
    instructions: Option<String>,
    compatibility: Option<String>,
    allowed_tools: Option<String>,
    metadata: BTreeMap<String, String>,
    load_error: Option<String>,
}

fn normalize_skill_list(
    raw_skills: Option<&Value>,
    workspace: &Path,
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

fn entries_from_object(
    object: &serde_json::Map<String, Value>,
    workspace: &Path,
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

fn entries_from_path(raw_path: &str, workspace: &Path, load_instructions: bool) -> Vec<SkillEntry> {
    if raw_path.is_empty() || !path_exists(raw_path, workspace) {
        return Vec::new();
    }
    let resolved = resolve_skill_path(raw_path, workspace);
    if resolved.is_dir() && find_skill_md(&resolved).is_none() {
        let mut entries = Vec::new();
        for skill_dir in discover_skill_dirs(&resolved) {
            entries.push(load_entry(&skill_dir, workspace, load_instructions));
        }
        return entries;
    }
    vec![load_entry(&resolved, workspace, load_instructions)]
}

fn path_exists(raw_path: &str, workspace: &Path) -> bool {
    let path = Path::new(raw_path);
    if path.exists() {
        return true;
    }
    if !path.is_absolute() {
        return workspace.join(path).exists();
    }
    false
}

fn resolve_skill_path(raw_path: &str, workspace: &Path) -> PathBuf {
    let path = PathBuf::from(raw_path);
    let path = if path.is_absolute() {
        path
    } else {
        workspace.join(path)
    };
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

fn discover_skill_dirs(root: &Path) -> Vec<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    let mut discovered = Vec::new();
    while let Some(path) = stack.pop() {
        if !path.is_dir() {
            continue;
        }
        if find_skill_md(&path).is_some() {
            discovered.push(path);
            continue;
        }
        if let Ok(entries) = std::fs::read_dir(&path) {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    stack.push(entry.path());
                }
            }
        }
    }
    discovered.sort();
    discovered
}

fn find_skill_md(skill_dir: &Path) -> Option<PathBuf> {
    for name in ["SKILL.md", "skill.md"] {
        let candidate = skill_dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn load_entry(skill_dir: &Path, workspace: &Path, load_instructions: bool) -> SkillEntry {
    match read_skill_file(skill_dir, workspace, load_instructions) {
        Ok(entry) => entry,
        Err(error) => SkillEntry {
            name: skill_dir
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("skill")
                .to_string(),
            description: String::new(),
            location: Some(skill_dir.display().to_string()),
            load_error: Some(error),
            ..SkillEntry::default()
        },
    }
}

fn read_skill_file(
    skill_dir: &Path,
    workspace: &Path,
    load_instructions: bool,
) -> Result<SkillEntry, String> {
    let skill_md = find_skill_md(skill_dir)
        .ok_or_else(|| format!("SKILL.md not found in {}", skill_dir.display()))?;
    let content = std::fs::read_to_string(&skill_md).map_err(|error| error.to_string())?;
    let (frontmatter, body) = parse_frontmatter(&content)?;
    let name = frontmatter
        .get("name")
        .cloned()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Missing required field in frontmatter: name".to_string())?;
    let description = frontmatter
        .get("description")
        .cloned()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Missing required field in frontmatter: description".to_string())?;
    Ok(SkillEntry {
        name,
        description,
        location: Some(relative_location(&skill_md, workspace)),
        instructions: load_instructions
            .then_some(body)
            .filter(|value| !value.is_empty()),
        compatibility: frontmatter.get("compatibility").cloned(),
        allowed_tools: frontmatter
            .get("allowed-tools")
            .or_else(|| frontmatter.get("allowed_tools"))
            .cloned(),
        metadata: frontmatter.metadata,
        load_error: None,
    })
}

#[derive(Default)]
struct ParsedFrontmatter {
    scalars: BTreeMap<String, String>,
    metadata: BTreeMap<String, String>,
}

impl ParsedFrontmatter {
    fn get(&self, key: &str) -> Option<&String> {
        self.scalars.get(key)
    }
}

fn parse_frontmatter(content: &str) -> Result<(ParsedFrontmatter, String), String> {
    let Some(rest) = content.strip_prefix("---") else {
        return Err("SKILL.md must start with YAML frontmatter (---)".to_string());
    };
    let Some((frontmatter, body)) = rest.split_once("\n---") else {
        return Err("SKILL.md frontmatter not properly closed with ---".to_string());
    };
    let mut parsed = ParsedFrontmatter::default();
    let mut in_metadata = false;
    for line in frontmatter.lines() {
        let trimmed = line.trim_end();
        if trimmed.trim().is_empty() || trimmed.trim_start().starts_with('#') {
            continue;
        }
        if trimmed == "metadata:" {
            in_metadata = true;
            continue;
        }
        if in_metadata && (trimmed.starts_with(' ') || trimmed.starts_with('\t')) {
            if let Some((key, value)) = trimmed.trim().split_once(':') {
                parsed
                    .metadata
                    .insert(key.trim().to_string(), clean_yaml_scalar(value));
            }
            continue;
        }
        in_metadata = false;
        if let Some((key, value)) = trimmed.split_once(':') {
            parsed
                .scalars
                .insert(key.trim().to_string(), clean_yaml_scalar(value));
        }
    }
    Ok((parsed, body.trim().to_string()))
}

fn clean_yaml_scalar(value: &str) -> String {
    let value = value.trim();
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })
        .unwrap_or(value)
        .trim()
        .to_string()
}

fn relative_location(skill_md: &Path, workspace: &Path) -> String {
    skill_md
        .strip_prefix(workspace)
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_else(|_| skill_md.to_string_lossy().to_string())
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

fn append_unique_string(state: &mut BTreeMap<String, Value>, key: &str, value: String) {
    let entry = state
        .entry(key.to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if !entry.is_array() {
        *entry = Value::Array(Vec::new());
    }
    if let Some(items) = entry.as_array_mut() {
        if !items.iter().any(|item| item.as_str() == Some(&value)) {
            items.push(Value::String(value));
        }
    }
}

fn append_activation_log(
    state: &mut BTreeMap<String, Value>,
    skill_name: String,
    reason: String,
    cycle_index: u32,
) {
    let entry = state
        .entry("skill_activation_log".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if !entry.is_array() {
        *entry = Value::Array(Vec::new());
    }
    if let Some(items) = entry.as_array_mut() {
        items.push(json!({
            "skill_name": skill_name,
            "reason": reason,
            "cycle_index": cycle_index,
        }));
    }
}
