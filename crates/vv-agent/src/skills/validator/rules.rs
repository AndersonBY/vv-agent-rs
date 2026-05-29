use std::path::Path;

use unicode_normalization::UnicodeNormalization;

use super::diagnostics::{append_issue, IssueSeverity, ValidationDiagnostics};
use super::mode::ValidationMode;

const MAX_SKILL_NAME_LENGTH: usize = 64;
const MAX_DESCRIPTION_LENGTH: usize = 1024;
pub(super) const ALLOWED_FIELDS: &[&str] = &[
    "name",
    "description",
    "license",
    "allowed-tools",
    "metadata",
];

pub(super) fn validate_name(
    name: &str,
    mode: ValidationMode,
    skill_dir: Option<&Path>,
) -> ValidationDiagnostics {
    let mut diagnostics = ValidationDiagnostics::default();
    let normalized = name.trim().nfkc().collect::<String>();
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
    if normalized != normalized.to_lowercase() {
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
        if let Some(raw_dir_name) = skill_dir.file_name().and_then(|name| name.to_str()) {
            let dir_name = raw_dir_name.nfkc().collect::<String>();
            if dir_name != normalized {
                append_issue(
                    &mut diagnostics,
                    format!("Directory name '{raw_dir_name}' must match skill name '{normalized}'"),
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

pub(super) fn validate_description(description: &str, diagnostics: &mut ValidationDiagnostics) {
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
