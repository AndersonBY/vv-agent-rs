use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use super::models::SkillEntry;
use super::parser::{entries_from_path, path_exists};

pub(super) fn normalize_skill_list(
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
