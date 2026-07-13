mod diagnostics;
mod mode;
mod rules;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::Value;

pub use self::diagnostics::ValidationDiagnostics;
use self::diagnostics::{append_issue, merge_diagnostics, IssueSeverity};
pub use self::mode::{
    normalize_validation_mode, ValidationMode, DEFAULT_VALIDATION_MODE, VALIDATION_MODES,
};
use self::rules::{validate_compatibility, validate_description, validate_name, ALLOWED_FIELDS};
use super::errors::SkillValidationError;
use super::parser::{find_skill_md, parse_frontmatter};

pub fn validate_metadata_with_diagnostics(
    metadata: &BTreeMap<String, Value>,
    skill_dir: Option<&Path>,
    validation_mode: Option<&str>,
) -> Result<ValidationDiagnostics, SkillValidationError> {
    let mode = normalize_validation_mode(validation_mode)?;
    let mut diagnostics = ValidationDiagnostics::default();

    let allowed = ALLOWED_FIELDS.iter().copied().collect::<BTreeSet<_>>();
    let extra_fields = metadata
        .keys()
        .filter(|key| !allowed.contains(key.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if !extra_fields.is_empty() {
        append_issue(
            &mut diagnostics,
            format!(
                "Unexpected fields in frontmatter: {}. Only {:?} are allowed.",
                extra_fields.join(", "),
                ALLOWED_FIELDS
            ),
            if mode == ValidationMode::Strict {
                IssueSeverity::Error
            } else {
                IssueSeverity::Warning
            },
        );
    }

    match metadata.get("name") {
        Some(Value::String(name)) => {
            merge_diagnostics(&mut diagnostics, validate_name(name, mode, skill_dir))
        }
        Some(_) => diagnostics
            .errors
            .push("Field 'name' must be a non-empty string".to_string()),
        None => diagnostics
            .errors
            .push("Missing required field in frontmatter: name".to_string()),
    }

    match metadata.get("description") {
        Some(Value::String(description)) => validate_description(description, &mut diagnostics),
        Some(_) => diagnostics
            .errors
            .push("Field 'description' must be a non-empty string".to_string()),
        None => diagnostics
            .errors
            .push("Missing required field in frontmatter: description".to_string()),
    }

    validate_compatibility(metadata.get("compatibility"), mode, &mut diagnostics);

    Ok(diagnostics)
}

pub fn validate_metadata(
    metadata: &BTreeMap<String, Value>,
    skill_dir: Option<&Path>,
    validation_mode: Option<&str>,
) -> Result<Vec<String>, SkillValidationError> {
    Ok(validate_metadata_with_diagnostics(metadata, skill_dir, validation_mode)?.errors)
}

pub fn validate_with_diagnostics(
    skill_dir: impl AsRef<Path>,
    validation_mode: Option<&str>,
) -> Result<ValidationDiagnostics, SkillValidationError> {
    let mode = normalize_validation_mode(validation_mode)?;
    let skill_dir = skill_dir.as_ref();
    let mut diagnostics = ValidationDiagnostics::default();
    if !skill_dir.exists() {
        diagnostics
            .errors
            .push(format!("Path does not exist: {}", skill_dir.display()));
        return Ok(diagnostics);
    }
    if !skill_dir.is_dir() {
        diagnostics
            .errors
            .push(format!("Not a directory: {}", skill_dir.display()));
        return Ok(diagnostics);
    }

    let Some(skill_md) = find_skill_md(skill_dir) else {
        diagnostics
            .errors
            .push("Missing required file: SKILL.md".to_string());
        return Ok(diagnostics);
    };
    let content =
        read_utf8_lossy(&skill_md).map_err(|error| SkillValidationError::new(error.to_string()))?;
    let (metadata, _) = match parse_frontmatter(&content) {
        Ok(parsed) => parsed,
        Err(error) => {
            diagnostics.errors.push(error.to_string());
            return Ok(diagnostics);
        }
    };
    validate_metadata_with_diagnostics(&metadata, Some(skill_dir), Some(mode.as_str()))
}

pub fn validate(
    skill_dir: impl AsRef<Path>,
    validation_mode: Option<&str>,
) -> Result<Vec<String>, SkillValidationError> {
    Ok(validate_with_diagnostics(skill_dir, validation_mode)?.errors)
}

fn read_utf8_lossy(path: &Path) -> Result<String, std::io::Error> {
    let bytes = std::fs::read(path)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
