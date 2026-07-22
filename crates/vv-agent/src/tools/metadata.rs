use std::cmp::Ordering;

use serde::{de::Error as _, Deserialize, Deserializer, Serialize};

use crate::checkpoint::ToolIdempotency;

pub const MAX_TOOL_METADATA_LABELS: usize = 32;
pub const MAX_TOOL_METADATA_LABEL_CODE_POINTS: usize = 128;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolSideEffect {
    #[default]
    Unknown,
    None,
    Read,
    Write,
    Execute,
    Network,
    External,
}

impl ToolSideEffect {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::None => "none",
            Self::Read => "read",
            Self::Write => "write",
            Self::Execute => "execute",
            Self::Network => "network",
            Self::External => "external",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ToolMetadata {
    pub side_effect: ToolSideEffect,
    pub idempotency: ToolIdempotency,
    pub terminal: bool,
    pub capability_tags: Vec<String>,
    pub cost_dimensions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("tool_metadata_invalid: {message}")]
pub struct ToolMetadataError {
    message: String,
}

impl ToolMetadataError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolMetadataWire {
    #[serde(default)]
    side_effect: ToolSideEffect,
    #[serde(default)]
    idempotency: ToolIdempotency,
    #[serde(default)]
    terminal: bool,
    #[serde(default)]
    capability_tags: Vec<String>,
    #[serde(default)]
    cost_dimensions: Vec<String>,
}

impl<'de> Deserialize<'de> for ToolMetadata {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ToolMetadataWire::deserialize(deserializer)?;
        Self {
            side_effect: wire.side_effect,
            idempotency: wire.idempotency,
            terminal: wire.terminal,
            capability_tags: wire.capability_tags,
            cost_dimensions: wire.cost_dimensions,
        }
        .normalized()
        .map_err(D::Error::custom)
    }
}

impl ToolMetadata {
    pub fn normalized(&self) -> Result<Self, ToolMetadataError> {
        Ok(Self {
            side_effect: self.side_effect,
            idempotency: self.idempotency,
            terminal: self.terminal,
            capability_tags: normalize_tool_metadata_labels(
                &self.capability_tags,
                "capability_tags",
            )?,
            cost_dimensions: normalize_tool_metadata_labels(
                &self.cost_dimensions,
                "cost_dimensions",
            )?,
        })
    }
}

pub(crate) fn normalize_tool_metadata_labels(
    values: &[String],
    field_name: &str,
) -> Result<Vec<String>, ToolMetadataError> {
    let mut normalized = Vec::with_capacity(values.len());
    for value in values {
        let value = trim_metadata_whitespace(value);
        if value.is_empty() {
            return Err(ToolMetadataError::new(format!(
                "{field_name} cannot contain a blank label"
            )));
        }
        if value.chars().count() > MAX_TOOL_METADATA_LABEL_CODE_POINTS {
            return Err(ToolMetadataError::new(format!(
                "{field_name} labels cannot exceed {MAX_TOOL_METADATA_LABEL_CODE_POINTS} Unicode code points"
            )));
        }
        normalized.push(value.to_string());
    }
    normalized.sort_by(|left, right| utf16_cmp(left, right));
    normalized.dedup();
    if normalized.len() > MAX_TOOL_METADATA_LABELS {
        return Err(ToolMetadataError::new(format!(
            "{field_name} cannot contain more than {MAX_TOOL_METADATA_LABELS} labels"
        )));
    }
    Ok(normalized)
}

fn trim_metadata_whitespace(value: &str) -> &str {
    value.trim_matches(|character| matches!(character, '\t' | '\n' | '\r' | ' '))
}

pub(crate) fn utf16_cmp(left: &str, right: &str) -> Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}
