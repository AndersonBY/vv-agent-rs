use super::common::*;
use super::*;

impl ToolExecutionResult {
    pub fn to_dict(&self) -> Value {
        let mut payload = serde_json::Map::from_iter([
            (
                "tool_call_id".to_string(),
                Value::String(self.tool_call_id.clone()),
            ),
            ("content".to_string(), Value::String(self.content.clone())),
            (
                "directive".to_string(),
                Value::String(tool_directive_value(self.directive).to_string()),
            ),
            (
                "status_code".to_string(),
                Value::String(tool_result_status_value(self.status).to_string()),
            ),
        ]);
        insert_optional_string(&mut payload, "error_code", &self.error_code);
        if !self.metadata.is_empty() {
            payload.insert("metadata".to_string(), metadata_to_value(&self.metadata));
        }
        insert_optional_string(&mut payload, "image_url", &self.image_url);
        insert_optional_string(&mut payload, "image_path", &self.image_path);
        if self.truncated {
            payload.insert("truncated".to_string(), Value::Bool(true));
        }
        if let Some(reason) = self.truncation_reason {
            payload.insert(
                "truncation_reason".to_string(),
                Value::String(
                    match reason {
                        ToolTruncationReason::OutputLimit => "output_limit",
                        ToolTruncationReason::ReadLimit => "read_limit",
                    }
                    .to_string(),
                ),
            );
        }
        if let Some(original_bytes) = self.original_bytes {
            payload.insert("original_bytes".to_string(), Value::from(original_bytes));
        }
        if let Some(visible_bytes) = self.visible_bytes {
            payload.insert("visible_bytes".to_string(), Value::from(visible_bytes));
        }
        if let Some(artifact) = &self.artifact {
            payload.insert(
                "artifact".to_string(),
                serde_json::to_value(artifact).expect("ToolArtifactRef is serializable"),
            );
        }
        if let Some(cursor) = &self.cursor {
            payload.insert(
                "cursor".to_string(),
                serde_json::to_value(cursor).expect("ToolResultCursor is serializable"),
            );
        }
        Value::Object(payload)
    }

    pub fn from_dict(data: &Value) -> Result<Self, String> {
        let object = expect_object(data, "ToolExecutionResult")?;
        let required = ["tool_call_id", "content", "status_code", "directive"];
        let allowed = [
            "tool_call_id",
            "content",
            "status_code",
            "directive",
            "error_code",
            "metadata",
            "image_url",
            "image_path",
            "truncated",
            "truncation_reason",
            "original_bytes",
            "visible_bytes",
            "artifact",
            "cursor",
        ];
        let mut missing = required
            .iter()
            .filter(|field| !object.contains_key(**field))
            .copied()
            .collect::<Vec<_>>();
        let mut unknown = object
            .keys()
            .filter(|field| !allowed.contains(&field.as_str()))
            .map(String::as_str)
            .collect::<Vec<_>>();
        missing.sort_unstable();
        unknown.sort_unstable();
        if !missing.is_empty() || !unknown.is_empty() {
            return Err(format!(
                "tool_result_invalid: ToolExecutionResult fields do not match the current wire: missing={missing:?}, unknown={unknown:?}"
            ));
        }

        let metadata = match object.get("metadata") {
            None => Metadata::new(),
            Some(Value::Object(metadata)) => metadata.clone().into_iter().collect(),
            Some(_) => return Err("ToolExecutionResult metadata must be an object".to_string()),
        };
        let truncated = match object.get("truncated") {
            None => false,
            Some(Value::Bool(true)) => true,
            Some(_) => {
                return Err("tool_result_invalid: truncated must be omitted or true".to_string())
            }
        };
        let truncation_reason = match strict_optional_string(object, "truncation_reason")?
            .as_deref()
        {
            None => None,
            Some("output_limit") => Some(ToolTruncationReason::OutputLimit),
            Some("read_limit") => Some(ToolTruncationReason::ReadLimit),
            Some(_) => return Err("tool_result_invalid: truncation_reason is invalid".to_string()),
        };
        let result = Self {
            tool_call_id: read_required_string(object, "tool_call_id")?.to_string(),
            content: read_required_string(object, "content")?.to_string(),
            status: parse_tool_result_status(read_required_string(object, "status_code")?)?,
            directive: parse_tool_directive(read_required_string(object, "directive")?)?,
            error_code: strict_optional_string(object, "error_code")?,
            metadata,
            image_url: strict_optional_string(object, "image_url")?,
            image_path: strict_optional_string(object, "image_path")?,
            truncated,
            truncation_reason,
            original_bytes: strict_optional_u64(object, "original_bytes")?,
            visible_bytes: strict_optional_u64(object, "visible_bytes")?,
            artifact: strict_optional_object(object, "artifact")?,
            cursor: strict_optional_object(object, "cursor")?,
        };
        result.validate()?;
        Ok(result)
    }
}

fn strict_optional_string(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<String>, String> {
    match object.get(key) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(format!("ToolExecutionResult {key} must be a string")),
    }
}

fn strict_optional_u64(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<u64>, String> {
    match object.get(key) {
        None => Ok(None),
        Some(Value::Number(value)) => value
            .as_u64()
            .filter(|value| *value <= crate::budget::MAX_WIRE_INTEGER)
            .map(Some)
            .ok_or_else(|| {
                format!("ToolExecutionResult {key} must be a JSON-safe unsigned integer")
            }),
        Some(_) => Err(format!(
            "ToolExecutionResult {key} must be a JSON-safe unsigned integer"
        )),
    }
}

fn strict_optional_object<T>(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<T>, String>
where
    T: serde::de::DeserializeOwned,
{
    match object.get(key) {
        None => Ok(None),
        Some(Value::Object(_)) => serde_json::from_value(object[key].clone())
            .map(Some)
            .map_err(|error| {
                format!("tool_result_invalid: ToolExecutionResult {key} is invalid: {error}")
            }),
        Some(_) => Err(format!("ToolExecutionResult {key} must be an object")),
    }
}
