use std::io::{Error, ErrorKind, Result};

use serde_json::Value;

use crate::budget::BudgetUsageSnapshot;
use crate::runtime::state::{checkpoint_status_from_value, checkpoint_status_value, Checkpoint};
use crate::types::{CycleRecord, Message, MessageRole};

pub(crate) fn checkpoint_to_json(checkpoint: &Checkpoint) -> Result<String> {
    validate_checkpoint(checkpoint)?;
    let mut payload = serde_json::Map::from_iter([
        (
            "task_id".to_string(),
            Value::String(checkpoint.task_id.clone()),
        ),
        (
            "cycle_index".to_string(),
            Value::from(checkpoint.cycle_index),
        ),
        (
            "status".to_string(),
            Value::String(checkpoint_status_value(checkpoint.status).to_string()),
        ),
        (
            "messages".to_string(),
            Value::Array(checkpoint.messages.iter().map(Message::to_dict).collect()),
        ),
        (
            "cycles".to_string(),
            Value::Array(checkpoint.cycles.iter().map(CycleRecord::to_dict).collect()),
        ),
        (
            "shared_state".to_string(),
            Value::Object(checkpoint.shared_state.clone().into_iter().collect()),
        ),
    ]);
    if checkpoint.revision != 0 {
        payload.insert("revision".to_string(), Value::from(checkpoint.revision));
    }
    if let Some(claim_token) = &checkpoint.claim_token {
        payload.insert(
            "claim_token".to_string(),
            Value::String(claim_token.clone()),
        );
    }
    if let Some(claimed_cycle) = checkpoint.claimed_cycle {
        payload.insert("claimed_cycle".to_string(), Value::from(claimed_cycle));
    }
    if let Some(lease_expires_at_ms) = checkpoint.lease_expires_at_ms {
        payload.insert(
            "lease_expires_at_ms".to_string(),
            Value::from(lease_expires_at_ms),
        );
    }
    if let Some(terminal_result) = &checkpoint.terminal_result {
        payload.insert("terminal_result".to_string(), terminal_result.to_dict());
    }
    if let Some(budget_usage) = &checkpoint.budget_usage {
        payload.insert(
            "budget_usage".to_string(),
            serde_json::to_value(budget_usage).map_err(json_to_io)?,
        );
    }
    serde_json::to_string(&Value::Object(payload)).map_err(json_to_io)
}

pub(crate) fn checkpoint_from_json(raw: &str) -> Result<Checkpoint> {
    let payload = serde_json::from_str::<Value>(raw).map_err(json_to_io)?;
    let object = payload.as_object().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            "checkpoint payload must be an object",
        )
    })?;
    let task_id = required_non_empty_string(object, "task_id")?.to_string();
    let cycle_index = required_u32(object, "cycle_index")?;
    let status = checkpoint_status_from_value(required_string(object, "status")?)?;
    let messages = required_array(object, "messages")?
        .iter()
        .enumerate()
        .map(|(index, value)| strict_message_from_dict(value, index))
        .collect::<Result<Vec<_>>>()?;
    let cycles = required_array(object, "cycles")?
        .iter()
        .enumerate()
        .map(|(index, value)| strict_cycle_from_dict(value, index))
        .collect::<Result<Vec<_>>>()?;
    let shared_state = object
        .get("shared_state")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidData,
                "checkpoint shared_state must be an object",
            )
        })?
        .clone()
        .into_iter()
        .collect();
    let revision = default_u64(object, "revision", 0)?;
    let claim_token = optional_non_empty_string(object, "claim_token")?;
    let claimed_cycle = optional_u32(object, "claimed_cycle")?;
    let lease_expires_at_ms = optional_u64(object, "lease_expires_at_ms")?;
    let terminal_result = object
        .get("terminal_result")
        .filter(|value| !value.is_null())
        .map(crate::types::AgentResult::from_dict)
        .transpose()
        .map_err(|error| {
            Error::new(
                ErrorKind::InvalidData,
                format!("invalid checkpoint terminal_result: {error}"),
            )
        })?;
    let budget_usage = object
        .get("budget_usage")
        .filter(|value| !value.is_null())
        .map(|value| serde_json::from_value::<BudgetUsageSnapshot>(value.clone()))
        .transpose()
        .map_err(|error| {
            Error::new(
                ErrorKind::InvalidData,
                format!("invalid checkpoint budget_usage: {error}"),
            )
        })?;
    let checkpoint = Checkpoint {
        task_id,
        cycle_index,
        status,
        messages,
        cycles,
        shared_state,
        revision,
        claim_token,
        claimed_cycle,
        lease_expires_at_ms,
        terminal_result,
        budget_usage,
    };
    validate_checkpoint(&checkpoint)?;
    Ok(checkpoint)
}

pub(crate) fn validate_checkpoint(checkpoint: &Checkpoint) -> Result<()> {
    if checkpoint.task_id.trim().is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "checkpoint task_id must be a non-empty string",
        ));
    }
    serde_json::to_value(&checkpoint.shared_state).map_err(json_to_io)?;
    for (index, message) in checkpoint.messages.iter().enumerate() {
        validate_message(message, index)?;
    }
    for (index, cycle) in checkpoint.cycles.iter().enumerate() {
        validate_cycle(cycle, index)?;
    }
    let claim_fields = [
        checkpoint.claim_token.is_some(),
        checkpoint.claimed_cycle.is_some(),
        checkpoint.lease_expires_at_ms.is_some(),
    ];
    if claim_fields.iter().any(|value| *value) && !claim_fields.iter().all(|value| *value) {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "checkpoint claim_token, claimed_cycle, and lease_expires_at_ms must be set together",
        ));
    }
    if checkpoint.terminal_result.is_some() && checkpoint.claim_token.is_some() {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "checkpoint terminal_result cannot have an active claim",
        ));
    }
    if let Some(claimed_cycle) = checkpoint.claimed_cycle {
        if claimed_cycle == 0 || claimed_cycle != checkpoint.cycle_index.saturating_add(1) {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "checkpoint claimed_cycle must be exactly cycle_index + 1",
            ));
        }
    }
    if checkpoint
        .terminal_result
        .as_ref()
        .is_some_and(|result| result.status != checkpoint.status)
    {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "checkpoint terminal_result status must match checkpoint status",
        ));
    }
    if let Some(budget_usage) = &checkpoint.budget_usage {
        budget_usage
            .validate()
            .map_err(|error| Error::new(ErrorKind::InvalidData, error))?;
    }
    Ok(())
}

fn strict_message_from_dict(value: &Value, index: usize) -> Result<Message> {
    let object = value.as_object().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            format!("checkpoint messages[{index}] must be an object"),
        )
    })?;
    required_string(object, "role")?;
    required_string(object, "content")?;
    if object
        .get("tool_calls")
        .is_some_and(|value| !value.is_array())
    {
        return invalid(format!(
            "checkpoint messages[{index}].tool_calls must be an array"
        ));
    }
    if object
        .get("metadata")
        .is_some_and(|value| !value.is_object())
    {
        return invalid(format!(
            "checkpoint messages[{index}].metadata must be an object"
        ));
    }
    for key in ["name", "tool_call_id", "reasoning_content", "image_url"] {
        if object
            .get(key)
            .is_some_and(|value| !value.is_null() && !value.is_string())
        {
            return invalid(format!(
                "checkpoint messages[{index}].{key} must be a string or null"
            ));
        }
    }
    let message = Message::from_dict(value).map_err(|error| {
        Error::new(
            ErrorKind::InvalidData,
            format!("invalid checkpoint messages[{index}]: {error}"),
        )
    })?;
    validate_message(&message, index)?;
    Ok(message)
}

fn strict_cycle_from_dict(value: &Value, index: usize) -> Result<CycleRecord> {
    let object = value.as_object().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            format!("checkpoint cycles[{index}] must be an object"),
        )
    })?;
    required_u32(object, "index")?;
    required_string(object, "assistant_message")?;
    required_array(object, "tool_calls")?;
    required_array(object, "tool_results")?;
    if !object
        .get("memory_compacted")
        .is_some_and(Value::is_boolean)
    {
        return invalid(format!(
            "checkpoint cycles[{index}].memory_compacted must be a boolean"
        ));
    }
    if !object.get("token_usage").is_some_and(Value::is_object) {
        return invalid(format!(
            "checkpoint cycles[{index}].token_usage must be an object"
        ));
    }
    let cycle = CycleRecord::from_dict(value).map_err(|error| {
        Error::new(
            ErrorKind::InvalidData,
            format!("invalid checkpoint cycles[{index}]: {error}"),
        )
    })?;
    validate_cycle(&cycle, index)?;
    Ok(cycle)
}

fn validate_message(message: &Message, index: usize) -> Result<()> {
    match message.role {
        MessageRole::System | MessageRole::User | MessageRole::Assistant | MessageRole::Tool => {}
    }
    serde_json::to_value(message.to_dict()).map_err(|error| {
        Error::new(
            ErrorKind::InvalidData,
            format!("invalid checkpoint messages[{index}]: {error}"),
        )
    })?;
    Ok(())
}

fn validate_cycle(cycle: &CycleRecord, index: usize) -> Result<()> {
    serde_json::to_value(cycle.to_dict()).map_err(|error| {
        Error::new(
            ErrorKind::InvalidData,
            format!("invalid checkpoint cycles[{index}]: {error}"),
        )
    })?;
    Ok(())
}

fn required_string<'a>(object: &'a serde_json::Map<String, Value>, key: &str) -> Result<&'a str> {
    object.get(key).and_then(Value::as_str).ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            format!("checkpoint {key} must be a string"),
        )
    })
}

fn required_non_empty_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    key: &str,
) -> Result<&'a str> {
    let value = required_string(object, key)?;
    if value.trim().is_empty() {
        return invalid(format!("checkpoint {key} must be a non-empty string"));
    }
    Ok(value)
}

fn required_u32(object: &serde_json::Map<String, Value>, key: &str) -> Result<u32> {
    object
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidData,
                format!("checkpoint {key} must be an unsigned 32-bit integer"),
            )
        })
}

fn optional_u32(object: &serde_json::Map<String, Value>, key: &str) -> Result<Option<u32>> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .map(Some)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidData,
                    format!("checkpoint {key} must be an unsigned 32-bit integer or null"),
                )
            }),
    }
}

fn optional_u64(object: &serde_json::Map<String, Value>, key: &str) -> Result<Option<u64>> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidData,
                format!("checkpoint {key} must be an unsigned 64-bit integer or null"),
            )
        }),
    }
}

fn default_u64(object: &serde_json::Map<String, Value>, key: &str, default: u64) -> Result<u64> {
    match object.get(key) {
        None => Ok(default),
        Some(value) => value.as_u64().ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidData,
                format!("checkpoint {key} must be an unsigned 64-bit integer"),
            )
        }),
    }
}

fn optional_non_empty_string(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<String>> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value.clone())),
        Some(_) => invalid(format!(
            "checkpoint {key} must be a non-empty string or null"
        )),
    }
}

fn required_array<'a>(
    object: &'a serde_json::Map<String, Value>,
    key: &str,
) -> Result<&'a Vec<Value>> {
    object.get(key).and_then(Value::as_array).ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            format!("checkpoint {key} must be an array"),
        )
    })
}

fn invalid<T>(message: String) -> Result<T> {
    Err(Error::new(ErrorKind::InvalidData, message))
}

fn json_to_io(error: serde_json::Error) -> Error {
    Error::new(ErrorKind::InvalidData, error)
}
