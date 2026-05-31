use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::types::{ToolDirective, ToolExecutionResult, ToolResultStatus};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolOutput {
    Text {
        text: String,
    },
    Json {
        value: Value,
    },
    Image {
        url: Option<String>,
        path: Option<PathBuf>,
        mime_type: Option<String>,
    },
    File {
        path: PathBuf,
        mime_type: Option<String>,
    },
    Error {
        message: String,
        code: Option<String>,
        retryable: bool,
    },
}

impl ToolOutput {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    pub fn json(value: Value) -> Self {
        Self::Json { value }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
            code: None,
            retryable: false,
        }
    }

    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        if let Self::Error { code: field, .. } = &mut self {
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

    pub fn to_result(&self, tool_call_id: impl Into<String>) -> ToolExecutionResult {
        let tool_call_id = tool_call_id.into();
        match self {
            Self::Text { text } => ToolExecutionResult::success(tool_call_id, text.clone()),
            Self::Json { value } => {
                let mut result = ToolExecutionResult::success(tool_call_id, value.to_string());
                result
                    .metadata
                    .insert("output_type".to_string(), json!("json"));
                result
            }
            Self::Image {
                url,
                path,
                mime_type,
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
                result
            }
            Self::File { path, mime_type } => {
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
                result
            }
            Self::Error {
                message,
                code,
                retryable,
            } => ToolExecutionResult {
                tool_call_id,
                content: json!({
                    "ok": false,
                    "error": message,
                    "error_code": code,
                    "retryable": retryable,
                })
                .to_string(),
                status: ToolResultStatus::Error,
                directive: ToolDirective::Continue,
                error_code: code.clone(),
                metadata: [
                    ("output_type".to_string(), json!("error")),
                    ("retryable".to_string(), json!(retryable)),
                ]
                .into_iter()
                .collect(),
                image_url: None,
                image_path: None,
            },
        }
    }
}
