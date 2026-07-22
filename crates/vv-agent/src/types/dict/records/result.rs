use serde_json::Value;

use crate::types::{AgentResult, CompletionReason, CycleRecord, Message};

use super::super::common::*;
use super::super::token_usage::{task_token_usage_from_dict, task_token_usage_to_dict};

const REQUIRED_FIELDS: [&str; 13] = [
    "status",
    "completion_reason",
    "completion_tool_name",
    "partial_output",
    "messages",
    "cycles",
    "final_answer",
    "wait_reason",
    "error",
    "shared_state",
    "token_usage",
    "checkpoint_key",
    "resume_observation",
];
const OPTIONAL_FIELDS: [&str; 3] = ["budget_usage", "budget_exhaustion", "error_code"];

impl AgentResult {
    pub fn to_dict(&self) -> Value {
        let mut payload = serde_json::Map::from_iter([
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
                "checkpoint_key".to_string(),
                self.checkpoint_key
                    .clone()
                    .map(Value::String)
                    .unwrap_or(Value::Null),
            ),
            (
                "resume_observation".to_string(),
                self.resume_observation
                    .as_ref()
                    .map(|observation| {
                        serde_json::to_value(observation)
                            .expect("validated resume observation always serializes")
                    })
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
        ]);
        if let Some(budget_usage) = &self.budget_usage {
            payload.insert(
                "budget_usage".to_string(),
                serde_json::to_value(budget_usage)
                    .expect("validated budget usage always serializes"),
            );
        }
        if let Some(budget_exhaustion) = &self.budget_exhaustion {
            payload.insert(
                "budget_exhaustion".to_string(),
                serde_json::to_value(budget_exhaustion)
                    .expect("validated budget exhaustion always serializes"),
            );
        }
        if let Some(error_code) = &self.error_code {
            payload.insert("error_code".to_string(), Value::String(error_code.clone()));
        }
        Value::Object(payload)
    }

    pub fn from_dict(data: &Value) -> Result<Self, String> {
        let object = expect_object(data, "AgentResult")?;
        if REQUIRED_FIELDS
            .iter()
            .any(|field| !object.contains_key(*field))
            || object.keys().any(|field| {
                !REQUIRED_FIELDS.contains(&field.as_str())
                    && !OPTIONAL_FIELDS.contains(&field.as_str())
            })
        {
            return Err("AgentResult must contain exactly the current wire fields".to_string());
        }
        for field in OPTIONAL_FIELDS {
            if object.get(field).is_some_and(Value::is_null) {
                return Err(format!(
                    "AgentResult optional field '{field}' must be omitted when absent"
                ));
            }
        }
        let messages = read_array(object, "messages")
            .ok_or_else(|| "AgentResult field 'messages' must be an array".to_string())?
            .iter()
            .map(Message::from_dict)
            .collect::<Result<Vec<_>, _>>()?;
        let cycles = read_array(object, "cycles")
            .ok_or_else(|| "AgentResult field 'cycles' must be an array".to_string())?
            .iter()
            .map(CycleRecord::from_dict)
            .collect::<Result<Vec<_>, _>>()?;
        let result = Self {
            status: parse_agent_status(read_required_string(object, "status")?)?,
            messages,
            cycles,
            completion_reason: optional_completion_reason(object)?,
            completion_tool_name: strict_optional_string(object, "completion_tool_name")?,
            partial_output: strict_optional_string(object, "partial_output")?,
            budget_usage: object
                .get("budget_usage")
                .map(|value| {
                    serde_json::from_value(value.clone()).map_err(|error| {
                        format!("AgentResult field 'budget_usage' must be a valid object: {error}")
                    })
                })
                .transpose()?,
            budget_exhaustion: object
                .get("budget_exhaustion")
                .map(|value| {
                    serde_json::from_value(value.clone()).map_err(|error| {
                        format!(
                            "AgentResult field 'budget_exhaustion' must be a valid object: {error}"
                        )
                    })
                })
                .transpose()?,
            checkpoint_key: strict_optional_string(object, "checkpoint_key")?,
            resume_observation: object
                .get("resume_observation")
                .filter(|value| !value.is_null())
                .map(|value| {
                    serde_json::from_value(value.clone()).map_err(|error| {
                        format!(
                            "AgentResult field 'resume_observation' must be a valid object: {error}"
                        )
                    })
                })
                .transpose()?,
            final_answer: strict_optional_string(object, "final_answer")?,
            wait_reason: strict_optional_string(object, "wait_reason")?,
            error: strict_optional_string(object, "error")?,
            error_code: strict_optional_string(object, "error_code")?,
            shared_state: read_metadata(object, "shared_state")?,
            token_usage: object
                .get("token_usage")
                .ok_or_else(|| "AgentResult field 'token_usage' is required".to_string())
                .and_then(task_token_usage_from_dict)?,
        };
        if result.to_dict() != *data {
            return Err("AgentResult must use the canonical current wire shape".to_string());
        }
        Ok(result)
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
