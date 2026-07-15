use serde_json::Value;

use crate::types::{AgentResult, CompletionReason, CycleRecord, Message};

use super::super::common::*;
use super::super::token_usage::{task_token_usage_from_dict, task_token_usage_to_dict};

impl AgentResult {
    pub fn to_dict(&self) -> Value {
        Value::Object(serde_json::Map::from_iter([
            (
                "status".to_string(),
                Value::String(agent_status_value(self.status).to_string()),
            ),
            (
                "completion_reason".to_string(),
                self.completion_reason
                    .map(completion_reason_value)
                    .map(str::to_string)
                    .map(Value::String)
                    .unwrap_or(Value::Null),
            ),
            (
                "completion_tool_name".to_string(),
                self.completion_tool_name
                    .clone()
                    .map(Value::String)
                    .unwrap_or(Value::Null),
            ),
            (
                "partial_output".to_string(),
                self.partial_output
                    .clone()
                    .map(Value::String)
                    .unwrap_or(Value::Null),
            ),
            (
                "messages".to_string(),
                Value::Array(self.messages.iter().map(Message::to_dict).collect()),
            ),
            (
                "cycles".to_string(),
                Value::Array(self.cycles.iter().map(CycleRecord::to_dict).collect()),
            ),
            (
                "final_answer".to_string(),
                self.final_answer
                    .clone()
                    .map(Value::String)
                    .unwrap_or(Value::Null),
            ),
            (
                "wait_reason".to_string(),
                self.wait_reason
                    .clone()
                    .map(Value::String)
                    .unwrap_or(Value::Null),
            ),
            (
                "error".to_string(),
                self.error.clone().map(Value::String).unwrap_or(Value::Null),
            ),
            (
                "shared_state".to_string(),
                metadata_to_value(&self.shared_state),
            ),
            (
                "token_usage".to_string(),
                task_token_usage_to_dict(&self.token_usage),
            ),
        ]))
    }

    pub fn from_dict(data: &Value) -> Result<Self, String> {
        let object = expect_object(data, "AgentResult")?;
        let messages = read_array(object, "messages")
            .unwrap_or(&[])
            .iter()
            .map(Message::from_dict)
            .collect::<Result<Vec<_>, _>>()?;
        let cycles = read_array(object, "cycles")
            .unwrap_or(&[])
            .iter()
            .map(CycleRecord::from_dict)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            status: parse_agent_status(read_required_string(object, "status")?)?,
            messages,
            cycles,
            completion_reason: optional_completion_reason(object)?,
            completion_tool_name: strict_optional_string(object, "completion_tool_name")?,
            partial_output: strict_optional_string(object, "partial_output")?,
            final_answer: read_optional_string(object, "final_answer"),
            wait_reason: read_optional_string(object, "wait_reason"),
            error: read_optional_string(object, "error"),
            shared_state: read_metadata(object, "shared_state")?,
            token_usage: object
                .get("token_usage")
                .map(task_token_usage_from_dict)
                .transpose()?
                .unwrap_or_default(),
        })
    }
}

fn optional_completion_reason(
    object: &serde_json::Map<String, Value>,
) -> Result<Option<CompletionReason>, String> {
    strict_optional_string(object, "completion_reason")?
        .as_deref()
        .map(parse_completion_reason)
        .transpose()
}

fn strict_optional_string(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<String>, String> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(format!(
            "AgentResult field '{key}' must be a string or null"
        )),
    }
}
