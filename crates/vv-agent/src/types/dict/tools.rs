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
                "status".to_string(),
                Value::String(tool_result_simple_status(self.status).to_string()),
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
        let status = match read_optional_string(object, "status_code").as_deref() {
            Some(status_code) => parse_tool_result_status(status_code)?,
            None => {
                let status =
                    read_optional_string(object, "status").unwrap_or_else(|| "success".to_string());
                parse_tool_result_status(&status)
                    .or_else(|_| parse_simple_tool_result_status(&status))?
            }
        };
        Ok(Self {
            tool_call_id: read_string(object, "tool_call_id").unwrap_or_default(),
            content: read_string(object, "content").unwrap_or_default(),
            status,
            directive: parse_tool_directive(
                read_optional_string(object, "directive")
                    .as_deref()
                    .unwrap_or("continue"),
            )?,
            error_code: read_optional_string(object, "error_code"),
            metadata: read_metadata(object, "metadata")?,
            image_url: read_optional_string(object, "image_url"),
            image_path: read_optional_string(object, "image_path"),
        })
    }
}
