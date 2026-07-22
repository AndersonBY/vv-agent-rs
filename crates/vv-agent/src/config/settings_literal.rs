use serde_json::Value;
use thiserror::Error;

mod assignment;
mod identifiers;
mod json;
mod strings;

#[derive(Debug, Error)]
pub enum SettingsLiteralError {
    #[error("cannot find LLM_SETTINGS or settings assignment")]
    MissingAssignment,
    #[error("invalid settings literal: {0}")]
    InvalidLiteral(String),
    #[error("failed to decode normalized settings literal as JSON: {0}")]
    Json(#[from] serde_json::Error),
}

pub(super) fn parse_llm_settings_source(source: &str) -> Result<Value, SettingsLiteralError> {
    let literal = assignment::extract_assignment_literal(source, &["LLM_SETTINGS"])?;
    let json_source = json::literal_to_json(literal)?;
    let value: Value = serde_json::from_str(&json_source)?;
    if value.is_object() {
        Ok(value)
    } else {
        Err(SettingsLiteralError::InvalidLiteral(
            "settings assignment must evaluate to a mapping".to_string(),
        ))
    }
}
