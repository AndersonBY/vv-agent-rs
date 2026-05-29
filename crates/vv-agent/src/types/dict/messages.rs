use super::common::*;
use super::*;

impl Message {
    pub fn to_openai_message(&self, include_reasoning_content: bool) -> Value {
        let mut payload = serde_json::Map::from_iter([
            (
                "role".to_string(),
                Value::String(message_role_value(self.role).to_string()),
            ),
            ("content".to_string(), Value::String(self.content.clone())),
        ]);
        insert_non_empty_optional_string(&mut payload, "name", &self.name);
        insert_non_empty_optional_string(&mut payload, "tool_call_id", &self.tool_call_id);
        if self.role == MessageRole::Assistant && !self.tool_calls.is_empty() {
            payload.insert(
                "tool_calls".to_string(),
                Value::Array(self.tool_calls.iter().map(tool_call_to_openai).collect()),
            );
            if self.content.is_empty() {
                payload.insert("content".to_string(), Value::Null);
            }
        }
        if include_reasoning_content && self.role == MessageRole::Assistant {
            insert_non_empty_optional_string(
                &mut payload,
                "reasoning_content",
                &self.reasoning_content,
            );
        }
        if self.role == MessageRole::User {
            if let Some(image_url) = self.image_url.as_deref().filter(|value| !value.is_empty()) {
                let mut blocks = Vec::new();
                if !self.content.is_empty() {
                    blocks.push(serde_json::json!({
                        "type": "text",
                        "text": self.content,
                    }));
                }
                blocks.push(serde_json::json!({
                    "type": "image_url",
                    "image_url": {"url": image_url},
                }));
                payload.insert("content".to_string(), Value::Array(blocks));
            }
        }
        Value::Object(payload)
    }

    pub fn to_dict(&self) -> Value {
        let mut payload = serde_json::Map::from_iter([
            (
                "role".to_string(),
                Value::String(message_role_value(self.role).to_string()),
            ),
            ("content".to_string(), Value::String(self.content.clone())),
        ]);
        insert_optional_string(&mut payload, "name", &self.name);
        insert_optional_string(&mut payload, "tool_call_id", &self.tool_call_id);
        if !self.tool_calls.is_empty() {
            payload.insert(
                "tool_calls".to_string(),
                Value::Array(self.tool_calls.iter().map(tool_call_to_openai).collect()),
            );
        }
        insert_optional_string(&mut payload, "reasoning_content", &self.reasoning_content);
        insert_optional_string(&mut payload, "image_url", &self.image_url);
        if !self.metadata.is_empty() {
            payload.insert("metadata".to_string(), metadata_to_value(&self.metadata));
        }
        Value::Object(payload)
    }

    pub fn from_dict(data: &Value) -> Result<Self, String> {
        let object = expect_object(data, "Message")?;
        let role = parse_message_role(read_required_string(object, "role")?)?;
        let tool_calls = read_array(object, "tool_calls")
            .unwrap_or(&[])
            .iter()
            .map(ToolCall::from_dict)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            role,
            content: read_string(object, "content").unwrap_or_default(),
            name: read_optional_string(object, "name"),
            tool_call_id: read_optional_string(object, "tool_call_id"),
            tool_calls,
            reasoning_content: read_optional_string(object, "reasoning_content"),
            image_url: read_optional_string(object, "image_url"),
            metadata: read_metadata(object, "metadata")?,
        })
    }
}

fn tool_call_to_openai(tool_call: &ToolCall) -> Value {
    let mut payload = serde_json::json!({
        "id": tool_call.id,
        "type": "function",
        "function": {
            "name": tool_call.name,
            "arguments": Value::Object(tool_call.arguments.clone().into_iter().collect()).to_string(),
        },
    });
    if let Some(extra_content) = &tool_call.extra_content {
        payload["extra_content"] = extra_content.clone();
    }
    payload
}

impl ToolCall {
    pub fn to_dict(&self) -> Value {
        let mut payload = serde_json::Map::from_iter([
            ("id".to_string(), Value::String(self.id.clone())),
            ("name".to_string(), Value::String(self.name.clone())),
            (
                "arguments".to_string(),
                Value::Object(self.arguments.clone().into_iter().collect()),
            ),
        ]);
        if let Some(extra_content) = &self.extra_content {
            payload.insert("extra_content".to_string(), extra_content.clone());
        }
        Value::Object(payload)
    }

    pub fn from_dict(data: &Value) -> Result<Self, String> {
        let object = expect_object(data, "ToolCall")?;
        if let Some(function) = object.get("function").and_then(Value::as_object) {
            let id = read_required_string(object, "id")?.to_string();
            let name = read_required_string(function, "name")?.to_string();
            let raw_arguments = function
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| Value::String("{}".to_string()));
            let mut tool_call = ToolCall::from_raw_arguments(id, name, raw_arguments);
            tool_call.extra_content = object
                .get("extra_content")
                .filter(|value| value.is_object())
                .cloned();
            return Ok(tool_call);
        }
        Ok(Self {
            id: read_required_string(object, "id")?.to_string(),
            name: read_required_string(object, "name")?.to_string(),
            arguments: read_metadata(object, "arguments")?,
            extra_content: object
                .get("extra_content")
                .filter(|value| value.is_object())
                .cloned(),
        })
    }
}
