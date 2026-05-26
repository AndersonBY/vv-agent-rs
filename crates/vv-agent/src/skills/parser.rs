use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::errors::{SkillError, SkillParseError, SkillValidationError};
use super::models::{LoadedSkill, SkillProperties};
use super::validator::validate_metadata_with_diagnostics;

pub fn find_skill_md(skill_dir: impl AsRef<Path>) -> Option<PathBuf> {
    let skill_dir = skill_dir.as_ref();
    for name in ["SKILL.md", "skill.md"] {
        let candidate = skill_dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

pub fn discover_skill_dirs(root: impl AsRef<Path>) -> Vec<PathBuf> {
    let root = root.as_ref();
    if !root.is_dir() {
        return Vec::new();
    }

    let mut discovered = Vec::new();
    let mut seen = BTreeSet::new();
    add_if_skill(root, &mut seen, &mut discovered);
    for candidate in recursive_dirs(root) {
        add_if_skill(&candidate, &mut seen, &mut discovered);
    }
    discovered
}

pub fn parse_frontmatter(
    content: &str,
) -> Result<(BTreeMap<String, Value>, String), SkillParseError> {
    let Some(rest) = content.strip_prefix("---") else {
        return Err(SkillParseError::new(
            "SKILL.md must start with YAML frontmatter (---)",
        ));
    };
    let Some((frontmatter, body)) = rest.split_once("---") else {
        return Err(SkillParseError::new(
            "SKILL.md frontmatter not properly closed with ---",
        ));
    };

    let parsed = serde_yaml::from_str::<Value>(frontmatter)
        .map_err(|error| SkillParseError::new(format!("Invalid YAML in frontmatter: {error}")))?;
    let Value::Object(object) = parsed else {
        return Err(SkillParseError::new(
            "SKILL.md frontmatter must be a YAML mapping",
        ));
    };

    let mut metadata = BTreeMap::new();
    for (key, value) in object {
        if key == "metadata" {
            metadata.insert(key, normalize_metadata_map(value));
        } else {
            metadata.insert(key, value);
        }
    }
    Ok((metadata, body.trim().to_string()))
}

pub fn read_properties(skill_dir: impl AsRef<Path>) -> Result<SkillProperties, SkillError> {
    let skill_dir = skill_dir.as_ref();
    let skill_md = find_skill_md(skill_dir).ok_or_else(|| {
        SkillParseError::new(format!("SKILL.md not found in {}", skill_dir.display()))
    })?;
    let content = read_utf8_lossy(&skill_md)?;
    let (metadata, _) = parse_frontmatter(&content)?;
    if !metadata.contains_key("name") {
        return Err(
            SkillValidationError::new("Missing required field in frontmatter: name").into(),
        );
    }
    if !metadata.contains_key("description") {
        return Err(SkillValidationError::new(
            "Missing required field in frontmatter: description",
        )
        .into());
    }
    build_properties(&metadata).map_err(Into::into)
}

pub fn read_skill(
    skill_dir: impl AsRef<Path>,
    validation_mode: Option<&str>,
) -> Result<LoadedSkill, SkillError> {
    let skill_dir = skill_dir.as_ref();
    let skill_md = find_skill_md(skill_dir).ok_or_else(|| {
        SkillParseError::new(format!("SKILL.md not found in {}", skill_dir.display()))
    })?;
    let content = read_utf8_lossy(&skill_md)?;
    let (metadata, body) = parse_frontmatter(&content)?;

    let diagnostics =
        validate_metadata_with_diagnostics(&metadata, Some(skill_dir), validation_mode)?;
    if !diagnostics.errors.is_empty() {
        return Err(SkillValidationError::new(diagnostics.errors.join("; ")).into());
    }

    let properties = build_properties(&metadata)?;
    Ok(LoadedSkill {
        properties,
        skill_md_path: skill_md.canonicalize().unwrap_or(skill_md),
        instructions: body,
        warnings: diagnostics.warnings,
    })
}

pub(crate) fn build_properties(
    metadata: &BTreeMap<String, Value>,
) -> Result<SkillProperties, SkillValidationError> {
    let name = required_string(metadata, "name")?;
    let description = required_string(metadata, "description")?;
    Ok(SkillProperties {
        name,
        description,
        license: optional_string(metadata.get("license")),
        compatibility: optional_string(metadata.get("compatibility")),
        allowed_tools: optional_string(
            metadata
                .get("allowed-tools")
                .or_else(|| metadata.get("allowed_tools")),
        ),
        metadata: metadata
            .get("metadata")
            .and_then(Value::as_object)
            .map(string_map_from_json_object)
            .unwrap_or_default(),
    })
}

fn add_if_skill(dir_path: &Path, seen: &mut BTreeSet<PathBuf>, discovered: &mut Vec<PathBuf>) {
    let normalized = dir_path
        .canonicalize()
        .unwrap_or_else(|_| dir_path.to_path_buf());
    if seen.contains(&normalized) || find_skill_md(&normalized).is_none() {
        return;
    }
    seen.insert(normalized.clone());
    discovered.push(normalized);
}

fn recursive_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut entries = match std::fs::read_dir(root) {
        Ok(entries) => entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>(),
        Err(_) => return dirs,
    };
    entries.sort();
    for entry in entries {
        dirs.push(entry.clone());
        dirs.extend(recursive_dirs(&entry));
    }
    dirs
}

fn normalize_metadata_map(value: Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .map(|(key, value)| (key, Value::String(value_to_string(&value))))
                .collect(),
        ),
        value => value,
    }
}

fn read_utf8_lossy(path: &Path) -> Result<String, SkillError> {
    let bytes = std::fs::read(path)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn required_string(
    metadata: &BTreeMap<String, Value>,
    key: &str,
) -> Result<String, SkillValidationError> {
    let Some(value) = metadata.get(key).and_then(Value::as_str).map(str::trim) else {
        return Err(SkillValidationError::new(format!(
            "Field '{key}' must be a non-empty string"
        )));
    };
    if value.is_empty() {
        return Err(SkillValidationError::new(format!(
            "Field '{key}' must be a non-empty string"
        )));
    }
    Ok(value.to_string())
}

fn optional_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn string_map_from_json_object(
    object: &serde_json::Map<String, Value>,
) -> BTreeMap<String, String> {
    object
        .iter()
        .map(|(key, value)| (key.clone(), value_to_string(value)))
        .collect()
}

fn value_to_string(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string())
}
