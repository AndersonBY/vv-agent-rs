use serde_json::{Map, Value};

use crate::types::{CycleRecord, ToolCall, ToolExecutionResult};

use super::super::common::*;

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
        ]))
    }

    pub fn from_dict(data: &Value) -> Result<Self, String> {
        let object = expect_object(data, "CycleRecord")?;
        let allowed = [
            "index",
            "assistant_message",
            "tool_calls",
            "tool_results",
            "memory_compacted",
        ];
        if let Some(field) = object
            .keys()
            .find(|field| !allowed.contains(&field.as_str()))
        {
            return Err(format!("CycleRecord contains unknown field {field:?}"));
        }
        let index = object
            .get("index")
            .and_then(Value::as_u64)
            .ok_or_else(|| "CycleRecord index must be an unsigned integer".to_string())?;
        let index = u32::try_from(index)
            .map_err(|_| "CycleRecord index must fit in an unsigned 32-bit integer".to_string())?;
        let assistant_message = object
            .get("assistant_message")
            .and_then(Value::as_str)
            .ok_or_else(|| "CycleRecord assistant_message must be a string".to_string())?
            .to_string();
        let tool_calls = object
            .get("tool_calls")
            .and_then(Value::as_array)
            .ok_or_else(|| "CycleRecord tool_calls must be an array".to_string())?
            .iter()
            .map(strict_tool_call)
            .collect::<Result<Vec<_>, _>>()?;
        let tool_results = object
            .get("tool_results")
            .and_then(Value::as_array)
            .ok_or_else(|| "CycleRecord tool_results must be an array".to_string())?
            .iter()
            .map(ToolExecutionResult::from_dict)
            .collect::<Result<Vec<_>, _>>()?;
        let memory_compacted = object
            .get("memory_compacted")
            .and_then(Value::as_bool)
            .ok_or_else(|| "CycleRecord memory_compacted must be a boolean".to_string())?;
        Ok(Self {
            index,
            assistant_message,
            tool_calls,
            tool_results,
            memory_compacted,
        })
    }
}

fn strict_tool_call(data: &Value) -> Result<ToolCall, String> {
    let object: &Map<String, Value> = expect_object(data, "ToolCall")?;
    let allowed = ["id", "name", "arguments", "extra_content"];
    if let Some(field) = object
        .keys()
        .find(|field| !allowed.contains(&field.as_str()))
    {
        return Err(format!("ToolCall contains unknown field {field:?}"));
    }
    let id = read_required_string(object, "id")?.to_string();
    let name = read_required_string(object, "name")?.to_string();
    let arguments = object
        .get("arguments")
        .and_then(Value::as_object)
        .cloned()
        .ok_or_else(|| "ToolCall arguments must be an object".to_string())?
        .into_iter()
        .collect();
    let extra_content = match object.get("extra_content") {
        None => None,
        Some(Value::Object(value)) => Some(Value::Object(value.clone())),
        Some(_) => return Err("ToolCall extra_content must be an object".to_string()),
    };
    Ok(ToolCall {
        id,
        name,
        arguments,
        extra_content,
    })
}
