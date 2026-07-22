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
                "ToolExecutionResult fields do not match the current wire: missing={missing:?}, unknown={unknown:?}"
            ));
        }

        let metadata = match object.get("metadata") {
            None => Metadata::new(),
            Some(Value::Object(metadata)) => metadata.clone().into_iter().collect(),
            Some(_) => return Err("ToolExecutionResult metadata must be an object".to_string()),
        };
        Ok(Self {
            tool_call_id: read_required_string(object, "tool_call_id")?.to_string(),
            content: read_required_string(object, "content")?.to_string(),
            status: parse_tool_result_status(read_required_string(object, "status_code")?)?,
            directive: parse_tool_directive(read_required_string(object, "directive")?)?,
            error_code: strict_optional_string(object, "error_code")?,
            metadata,
            image_url: strict_optional_string(object, "image_url")?,
            image_path: strict_optional_string(object, "image_path")?,
        })
    }
}

fn strict_optional_string(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<String>, String> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(format!(
            "ToolExecutionResult {key} must be a string or null"
        )),
    }
}
