use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Message, Metadata, ToolArguments, ToolDirective, ToolResultStatus};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: ToolArguments,
    pub extra_content: Option<Value>,
}

impl ToolCall {
    pub fn new(id: impl Into<String>, name: impl Into<String>, arguments: ToolArguments) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments,
            extra_content: None,
        }
    }

    pub fn from_raw_arguments(
        id: impl Into<String>,
        name: impl Into<String>,
        raw_arguments: Value,
    ) -> Self {
        let id = id.into();
        let name = name.into();
        match parse_raw_tool_arguments(&raw_arguments) {
            Ok(arguments) => Self {
                id,
                name,
                arguments,
                extra_content: None,
            },
            Err((error_code, error)) => Self {
                id,
                name,
                arguments: ToolArguments::new(),
                extra_content: Some(Value::Object(
                    [
                        ("raw_arguments".to_string(), raw_arguments),
                        ("argument_error_code".to_string(), Value::String(error_code)),
                        ("argument_error".to_string(), Value::String(error)),
                    ]
                    .into_iter()
                    .collect(),
                )),
            },
        }
    }
}

fn parse_raw_tool_arguments(raw_arguments: &Value) -> Result<ToolArguments, (String, String)> {
    match raw_arguments {
        Value::Null => Ok(ToolArguments::new()),
        Value::Object(object) => Ok(object.clone().into_iter().collect()),
        Value::String(raw) => {
            let stripped = raw.trim();
            if stripped.is_empty() {
                return Ok(ToolArguments::new());
            }
            let parsed = serde_json::from_str::<Value>(stripped).map_err(|error| {
                (
                    "invalid_arguments_json".to_string(),
                    format!("Invalid tool arguments JSON: {error}"),
                )
            })?;
            match parsed {
                Value::Object(object) => Ok(object.into_iter().collect()),
                _ => Err((
                    "invalid_arguments_payload".to_string(),
                    "Tool arguments must decode to an object".to_string(),
                )),
            }
        }
        other => Err((
            "invalid_arguments_type".to_string(),
            format!("Unsupported tool argument type: {}", json_type_name(other)),
        )),
    }
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolExecutionResult {
    pub tool_call_id: String,
    pub content: String,
    pub status: ToolResultStatus,
    pub directive: ToolDirective,
    pub error_code: Option<String>,
    pub metadata: Metadata,
    pub image_url: Option<String>,
    pub image_path: Option<String>,
    pub truncated: bool,
    pub truncation_reason: Option<ToolTruncationReason>,
    pub original_bytes: Option<u64>,
    pub visible_bytes: Option<u64>,
    pub artifact: Option<ToolArtifactRef>,
    pub cursor: Option<ToolResultCursor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolTruncationReason {
    OutputLimit,
    ReadLimit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolArtifactRef {
    pub path: String,
    pub media_type: String,
    pub encoding: String,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolResultCursor {
    pub kind: String,
    pub path: String,
    pub offset_chars: u64,
    pub sha256: String,
}

impl Serialize for ToolExecutionResult {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.validate().map_err(serde::ser::Error::custom)?;
        self.to_dict().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ToolExecutionResult {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        Self::from_dict(&value).map_err(serde::de::Error::custom)
    }
}

impl ToolExecutionResult {
    pub fn success(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            content: content.into(),
            status: ToolResultStatus::Success,
            directive: ToolDirective::Continue,
            error_code: None,
            metadata: Metadata::new(),
            image_url: None,
            image_path: None,
            truncated: false,
            truncation_reason: None,
            original_bytes: None,
            visible_bytes: None,
            artifact: None,
            cursor: None,
        }
    }

    pub fn error(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            status: ToolResultStatus::Error,
            ..Self::success(tool_call_id, content)
        }
    }

    pub fn with_error_code(mut self, error_code: impl Into<String>) -> Self {
        self.error_code = Some(error_code.into());
        self
    }

    pub fn to_message(&self) -> Message {
        let mut content = self.content.clone();
        if self.truncated {
            let recovery = self.recovery_value();
            let encoded = serde_json_canonicalizer::to_string(&recovery)
                .expect("validated tool recovery fields are canonical JSON");
            content.push('\n');
            content.push_str(&encoded);
        }
        let mut message = Message::tool(content, self.tool_call_id.clone());
        message.artifact_ref = self.artifact.clone();
        if self.artifact.is_some() {
            message.metadata.insert(
                "_vv_agent_microcompact_excerpt".to_string(),
                Value::String(self.content.clone()),
            );
        }
        message
    }

    pub fn to_tool_message(&self) -> Message {
        self.to_message()
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.status == ToolResultStatus::Success && self.error_code.is_some() {
            return Err(
                "tool_result_invalid: SUCCESS results must not contain an error_code".to_string(),
            );
        }
        for forbidden in ["content", "instructions", "output", "stderr", "stdout"] {
            if self.metadata.contains_key(forbidden) {
                return Err(format!(
                    "tool_result_invalid: metadata key {forbidden:?} may not repeat bulk output"
                ));
            }
        }
        if !self.truncated {
            if self.truncation_reason.is_some()
                || self.original_bytes.is_some()
                || self.visible_bytes.is_some()
                || self.artifact.is_some()
                || self.cursor.is_some()
            {
                return Err(
                    "tool_result_invalid: ordinary results cannot contain recovery fields"
                        .to_string(),
                );
            }
            return Ok(());
        }

        let reason = self.truncation_reason.ok_or_else(|| {
            "tool_result_invalid: truncated result requires truncation_reason".to_string()
        })?;
        let original_bytes = self.original_bytes.ok_or_else(|| {
            "tool_result_invalid: truncated result requires original_bytes".to_string()
        })?;
        let visible_bytes = self.visible_bytes.ok_or_else(|| {
            "tool_result_invalid: truncated result requires visible_bytes".to_string()
        })?;
        if visible_bytes != self.content.len() as u64 || visible_bytes > original_bytes {
            return Err(
                "tool_result_invalid: truncated result byte counts are invalid".to_string(),
            );
        }
        match reason {
            ToolTruncationReason::OutputLimit => {
                let artifact = self.artifact.as_ref().ok_or_else(|| {
                    "tool_result_invalid: output_limit requires artifact".to_string()
                })?;
                if self.cursor.is_some() {
                    return Err(
                        "tool_result_invalid: output_limit cannot contain cursor".to_string()
                    );
                }
                artifact.validate()?;
            }
            ToolTruncationReason::ReadLimit => {
                let cursor = self
                    .cursor
                    .as_ref()
                    .ok_or_else(|| "tool_result_invalid: read_limit requires cursor".to_string())?;
                if self.artifact.is_some() {
                    return Err(
                        "tool_result_invalid: read_limit cannot contain artifact".to_string()
                    );
                }
                cursor.validate()?;
            }
        }
        Ok(())
    }

    fn recovery_value(&self) -> Value {
        let mut recovery = serde_json::Map::new();
        recovery.insert("truncated".to_string(), Value::Bool(true));
        recovery.insert(
            "truncation_reason".to_string(),
            Value::String(
                match self.truncation_reason.expect("validated truncated result") {
                    ToolTruncationReason::OutputLimit => "output_limit",
                    ToolTruncationReason::ReadLimit => "read_limit",
                }
                .to_string(),
            ),
        );
        recovery.insert(
            "original_bytes".to_string(),
            Value::from(self.original_bytes.expect("validated truncated result")),
        );
        recovery.insert(
            "visible_bytes".to_string(),
            Value::from(self.visible_bytes.expect("validated truncated result")),
        );
        if let Some(artifact) = &self.artifact {
            recovery.insert(
                "artifact".to_string(),
                serde_json::to_value(artifact).expect("ToolArtifactRef is serializable"),
            );
        }
        if let Some(cursor) = &self.cursor {
            recovery.insert(
                "cursor".to_string(),
                serde_json::to_value(cursor).expect("ToolResultCursor is serializable"),
            );
        }
        Value::Object(serde_json::Map::from_iter([(
            "vv_agent_recovery".to_string(),
            Value::Object(recovery),
        )]))
    }
}

impl ToolArtifactRef {
    pub fn validate(&self) -> Result<(), String> {
        if !valid_artifact_path(&self.path) {
            return Err("artifact_path_invalid".to_string());
        }
        if self.media_type != "text/plain" || self.encoding != "utf-8" {
            return Err(
                "tool_result_invalid: artifact media type or encoding is invalid".to_string(),
            );
        }
        if !valid_sha256(&self.sha256) {
            return Err("tool_result_invalid: artifact sha256 is invalid".to_string());
        }
        Ok(())
    }
}

impl ToolResultCursor {
    pub fn validate(&self) -> Result<(), String> {
        if self.kind != "read_file"
            || self.path.trim().is_empty()
            || self.path.contains(['\\', '\0'])
            || self.offset_chars > crate::budget::MAX_WIRE_INTEGER
            || !valid_sha256(&self.sha256)
        {
            return Err("tool_result_invalid: read_file cursor is invalid".to_string());
        }
        Ok(())
    }
}

fn valid_artifact_path(path: &str) -> bool {
    const PREFIX: &str = ".vv-agent/artifacts/";
    if path.len() > 512 || !path.starts_with(PREFIX) || path.contains(['\\', '\0']) {
        return false;
    }
    path[PREFIX.len()..].split('/').all(|segment| {
        !segment.is_empty()
            && segment.len() <= 128
            && segment
                .bytes()
                .next()
                .is_some_and(|byte| byte.is_ascii_alphanumeric())
            && segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    })
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}
