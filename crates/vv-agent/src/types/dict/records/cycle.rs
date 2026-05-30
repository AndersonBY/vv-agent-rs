use serde_json::Value;

use crate::types::{CycleRecord, ToolCall, ToolExecutionResult};

use super::super::common::*;
use super::super::token_usage::{token_usage_from_dict, token_usage_to_dict};

impl CycleRecord {
    pub fn to_dict(&self) -> Value {
        Value::Object(serde_json::Map::from_iter([
            ("index".to_string(), Value::from(self.index)),
            (
                "assistant_message".to_string(),
                Value::String(self.assistant_message.clone()),
            ),
            (
                "tool_calls".to_string(),
                Value::Array(self.tool_calls.iter().map(ToolCall::to_dict).collect()),
            ),
            (
                "tool_results".to_string(),
                Value::Array(
                    self.tool_results
                        .iter()
                        .map(ToolExecutionResult::to_dict)
                        .collect(),
                ),
            ),
            (
                "memory_compacted".to_string(),
                Value::Bool(self.memory_compacted),
            ),
            (
                "token_usage".to_string(),
                token_usage_to_dict(&self.token_usage),
            ),
        ]))
    }

    pub fn from_dict(data: &Value) -> Result<Self, String> {
        let object = expect_object(data, "CycleRecord")?;
        let tool_calls = read_array(object, "tool_calls")
            .unwrap_or(&[])
            .iter()
            .map(ToolCall::from_dict)
            .collect::<Result<Vec<_>, _>>()?;
        let tool_results = read_array(object, "tool_results")
            .unwrap_or(&[])
            .iter()
            .map(ToolExecutionResult::from_dict)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            index: read_u32(object, "index", 0),
            assistant_message: read_string(object, "assistant_message").unwrap_or_default(),
            tool_calls,
            tool_results,
            memory_compacted: read_bool(object, "memory_compacted", false),
            token_usage: object
                .get("token_usage")
                .map(token_usage_from_dict)
                .transpose()?
                .unwrap_or_default(),
        })
    }
}
