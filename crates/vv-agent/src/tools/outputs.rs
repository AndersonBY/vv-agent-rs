use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::types::{ToolDirective, ToolExecutionResult, ToolResultStatus};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolOutput {
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        metadata: BTreeMap<String, Value>,
    },
    Json {
        data: Value,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        metadata: BTreeMap<String, Value>,
    },
    Image {
        url: Option<String>,
        path: Option<PathBuf>,
        mime_type: Option<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        metadata: BTreeMap<String, Value>,
    },
    File {
        path: PathBuf,
        mime_type: Option<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        metadata: BTreeMap<String, Value>,
    },
    Error {
        message: String,
        error_code: Option<String>,
        retryable: bool,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        metadata: BTreeMap<String, Value>,
    },
}

impl ToolOutput {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text {
            text: text.into(),
            metadata: BTreeMap::new(),
        }
    }

    pub fn json(data: Value) -> Self {
        Self::Json {
            data,
            metadata: BTreeMap::new(),
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
            error_code: None,
            retryable: false,
            metadata: BTreeMap::new(),
        }
    }

    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        if let Self::Error {
            error_code: field, ..
        } = &mut self
        {
            *field = Some(code.into());
        }
        self
    }

    pub fn retryable(mut self, retryable: bool) -> Self {
        if let Self::Error {
            retryable: field, ..
        } = &mut self
        {
            *field = retryable;
        }
        self
    }

    pub fn with_metadata(mut self, key: impl Into<String>, value: Value) -> Self {
        match &mut self {
            Self::Text { metadata, .. }
            | Self::Json { metadata, .. }
            | Self::Image { metadata, .. }
            | Self::File { metadata, .. }
            | Self::Error { metadata, .. } => {
                metadata.insert(key.into(), value);
            }
        }
        self
    }

    pub fn to_result(&self, tool_call_id: impl Into<String>) -> ToolExecutionResult {
        let tool_call_id = tool_call_id.into();
        match self {
            Self::Text { text, metadata } => {
                let mut result = ToolExecutionResult::success(tool_call_id, text.clone());
                result.metadata.extend(metadata.clone());
                result
            }
            Self::Json { data, metadata } => {
                let mut result = ToolExecutionResult::success(tool_call_id, data.to_string());
                result
                    .metadata
                    .insert("output_type".to_string(), json!("json"));
                result.metadata.extend(metadata.clone());
                result
            }
            Self::Image {
                url,
                path,
                mime_type,
                metadata,
            } => {
                let mut result = ToolExecutionResult::success(
                    tool_call_id,
                    json!({
                        "url": url,
                        "path": path,
                        "mime_type": mime_type,
                    })
                    .to_string(),
                );
                result
                    .metadata
                    .insert("output_type".to_string(), json!("image"));
                result.image_url = url.clone();
                result.image_path = path.as_ref().map(|path| path.display().to_string());
                result.metadata.extend(metadata.clone());
                result
            }
            Self::File {
                path,
                mime_type,
                metadata,
            } => {
                let mut result = ToolExecutionResult::success(
                    tool_call_id,
                    json!({
                        "path": path,
                        "mime_type": mime_type,
                    })
                    .to_string(),
                );
                result
                    .metadata
                    .insert("output_type".to_string(), json!("file"));
                result.metadata.extend(metadata.clone());
                result
            }
            Self::Error {
                message,
                error_code,
                retryable,
                metadata,
            } => ToolExecutionResult {
                tool_call_id,
                content: json!({
                    "ok": false,
                    "error": message,
                    "error_code": error_code,
                    "retryable": retryable,
                })
                .to_string(),
                status: ToolResultStatus::Error,
                directive: ToolDirective::Continue,
                error_code: error_code.clone(),
                metadata: [
                    ("output_type".to_string(), json!("error")),
                    ("retryable".to_string(), json!(retryable)),
                ]
                .into_iter()
                .chain(metadata.clone())
                .collect(),
                image_url: None,
                image_path: None,
            },
        }
    }
}
