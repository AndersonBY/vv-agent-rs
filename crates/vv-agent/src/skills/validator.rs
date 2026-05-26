use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::Value;

use super::errors::SkillValidationError;

const MAX_SKILL_NAME_LENGTH: usize = 64;
const MAX_DESCRIPTION_LENGTH: usize = 1024;
const MAX_COMPATIBILITY_LENGTH: usize = 500;
const ALLOWED_FIELDS: &[&str] = &[
    "name",
    "description",
    "license",
    "compatibility",
    "allowed-tools",
    "metadata",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationMode {
    Strict,
    Compat,
    Minimal,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValidationDiagnostics {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

pub fn normalize_validation_mode(
    validation_mode: Option<&str>,
) -> Result<ValidationMode, SkillValidationError> {
    match validation_mode
        .unwrap_or("strict")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "strict" => Ok(ValidationMode::Strict),
        "compat" => Ok(ValidationMode::Compat),
        "minimal" => Ok(ValidationMode::Minimal),
        other => Err(SkillValidationError::new(format!(
            "Unsupported validation mode '{other}'. Expected one of [strict, compat, minimal]."
        ))),
    }
}

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

    match metadata.get("name").and_then(Value::as_str) {
        Some(name) => merge_diagnostics(&mut diagnostics, validate_name(name, mode, skill_dir)),
        None => diagnostics
            .errors
            .push("Missing required field in frontmatter: name".to_string()),
    }

    match metadata.get("description").and_then(Value::as_str) {
        Some(description) => validate_description(description, &mut diagnostics),
        None => diagnostics
            .errors
            .push("Missing required field in frontmatter: description".to_string()),
    }

    if let Some(compatibility) = metadata.get("compatibility") {
        validate_compatibility(compatibility, mode, &mut diagnostics);
    }
    Ok(diagnostics)
}

pub fn validate_metadata(
    metadata: &BTreeMap<String, Value>,
    skill_dir: Option<&Path>,
    validation_mode: Option<&str>,
) -> Result<Vec<String>, SkillValidationError> {
    Ok(validate_metadata_with_diagnostics(metadata, skill_dir, validation_mode)?.errors)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IssueSeverity {
    Error,
    Warning,
}

fn append_issue(
    diagnostics: &mut ValidationDiagnostics,
    message: impl Into<String>,
    severity: IssueSeverity,
) {
    match severity {
        IssueSeverity::Error => diagnostics.errors.push(message.into()),
        IssueSeverity::Warning => diagnostics.warnings.push(message.into()),
    }
}

fn merge_diagnostics(base: &mut ValidationDiagnostics, incoming: ValidationDiagnostics) {
    base.errors.extend(incoming.errors);
    base.warnings.extend(incoming.warnings);
}

fn validate_name(
    name: &str,
    mode: ValidationMode,
    skill_dir: Option<&Path>,
) -> ValidationDiagnostics {
    let mut diagnostics = ValidationDiagnostics::default();
    let normalized = name.trim();
    if normalized.is_empty() {
        diagnostics
            .errors
            .push("Field 'name' must be a non-empty string".to_string());
        return diagnostics;
    }
    if normalized.chars().count() > MAX_SKILL_NAME_LENGTH {
        diagnostics.errors.push(format!(
            "Skill name '{normalized}' exceeds {MAX_SKILL_NAME_LENGTH} character limit"
        ));
    }
    if !normalized
        .chars()
        .all(|ch| ch.is_alphanumeric() || ch == '-')
    {
        diagnostics.errors.push(format!(
            "Skill name '{normalized}' contains invalid characters. Only letters, digits, and hyphens are allowed."
        ));
    }

    let naming_severity = if mode == ValidationMode::Minimal {
        IssueSeverity::Warning
    } else {
        IssueSeverity::Error
    };
    if normalized != normalized.to_ascii_lowercase() {
        append_issue(
            &mut diagnostics,
            format!("Skill name '{normalized}' must be lowercase"),
            naming_severity,
        );
    }
    if normalized.starts_with('-') || normalized.ends_with('-') {
        append_issue(
            &mut diagnostics,
            "Skill name cannot start or end with a hyphen",
            naming_severity,
        );
    }
    if normalized.contains("--") {
        append_issue(
            &mut diagnostics,
            "Skill name cannot contain consecutive hyphens",
            naming_severity,
        );
    }

    if let Some(skill_dir) = skill_dir {
        if let Some(dir_name) = skill_dir.file_name().and_then(|name| name.to_str()) {
            if dir_name != normalized {
                append_issue(
                    &mut diagnostics,
                    format!("Directory name '{dir_name}' must match skill name '{normalized}'"),
                    if mode == ValidationMode::Strict {
                        IssueSeverity::Error
                    } else {
                        IssueSeverity::Warning
                    },
                );
            }
        }
    }
    diagnostics
}

fn validate_description(description: &str, diagnostics: &mut ValidationDiagnostics) {
    if description.trim().is_empty() {
        diagnostics
            .errors
            .push("Field 'description' must be a non-empty string".to_string());
    }
    if description.chars().count() > MAX_DESCRIPTION_LENGTH {
        diagnostics.errors.push(format!(
            "Description exceeds {MAX_DESCRIPTION_LENGTH} character limit"
        ));
    }
}

fn validate_compatibility(
    compatibility: &Value,
    mode: ValidationMode,
    diagnostics: &mut ValidationDiagnostics,
) {
    let severity = if mode == ValidationMode::Minimal {
        IssueSeverity::Warning
    } else {
        IssueSeverity::Error
    };
    let Some(compatibility) = compatibility.as_str() else {
        append_issue(
            diagnostics,
            "Field 'compatibility' must be a string",
            severity,
        );
        return;
    };
    if compatibility.chars().count() > MAX_COMPATIBILITY_LENGTH {
        append_issue(
            diagnostics,
            format!("Compatibility exceeds {MAX_COMPATIBILITY_LENGTH} character limit"),
            severity,
        );
    }
}
