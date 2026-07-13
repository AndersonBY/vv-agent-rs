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
}

impl Serialize for ToolExecutionResult {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
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
        }
    }

    pub fn error(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            status: ToolResultStatus::Error,
            ..Self::success(tool_call_id, content)
        }
    }

    pub fn to_message(&self) -> Message {
        Message::tool(self.content.clone(), self.tool_call_id.clone())
    }

    pub fn to_tool_message(&self) -> Message {
        self.to_message()
    }
}
