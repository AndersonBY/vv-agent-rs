use super::super::errors::SkillValidationError;

pub const DEFAULT_VALIDATION_MODE: &str = "strict";
pub const VALIDATION_MODES: [&str; 3] = ["strict", "compat", "minimal"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationMode {
    Strict,
    Compat,
    Minimal,
}

impl ValidationMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Strict => "strict",
            Self::Compat => "compat",
            Self::Minimal => "minimal",
        }
    }
}

pub fn normalize_validation_mode(
    validation_mode: Option<&str>,
) -> Result<ValidationMode, SkillValidationError> {
    match validation_mode
        .unwrap_or(DEFAULT_VALIDATION_MODE)
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
