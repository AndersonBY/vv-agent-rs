use std::path::Path;

use crate::skills::errors::{SkillError, SkillParseError, SkillValidationError};
use crate::skills::models::{LoadedSkill, SkillProperties};
use crate::skills::validator::validate_metadata_with_diagnostics;

use super::discovery::find_skill_md;
use super::frontmatter::parse_frontmatter;
use super::io::read_utf8_lossy;
use super::properties::build_properties;

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
