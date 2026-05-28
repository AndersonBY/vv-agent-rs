use std::io::{Error, ErrorKind, Result};

use serde_json::{json, Value};

use crate::runtime::state::{checkpoint_status_from_value, checkpoint_status_value, Checkpoint};
use crate::types::{CycleRecord, Message};

pub(crate) fn messages_to_json(messages: &[Message]) -> Result<String> {
    serde_json::to_string(
        &messages
            .iter()
            .map(Message::to_dict)
            .collect::<Vec<Value>>(),
    )
    .map_err(json_to_io)
}

pub(crate) fn messages_from_json(raw: &str) -> Result<Vec<Message>> {
    let values = serde_json::from_str::<Vec<Value>>(raw).map_err(json_to_io)?;
    values
        .iter()
        .map(Message::from_dict)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|error| Error::new(ErrorKind::InvalidData, error))
}

pub(crate) fn cycles_to_json(cycles: &[CycleRecord]) -> Result<String> {
    serde_json::to_string(
        &cycles
            .iter()
            .map(CycleRecord::to_dict)
            .collect::<Vec<Value>>(),
    )
    .map_err(json_to_io)
}

pub(crate) fn cycles_from_json(raw: &str) -> Result<Vec<CycleRecord>> {
    let values = serde_json::from_str::<Vec<Value>>(raw).map_err(json_to_io)?;
    values
        .iter()
        .map(CycleRecord::from_dict)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|error| Error::new(ErrorKind::InvalidData, error))
}

pub(crate) fn checkpoint_to_json(checkpoint: &Checkpoint) -> Result<String> {
    let payload = json!({
        "task_id": checkpoint.task_id,
        "cycle_index": checkpoint.cycle_index,
        "status": checkpoint_status_value(checkpoint.status),
        "messages": checkpoint.messages.iter().map(Message::to_dict).collect::<Vec<_>>(),
        "cycles": checkpoint.cycles.iter().map(CycleRecord::to_dict).collect::<Vec<_>>(),
        "shared_state": checkpoint.shared_state,
    });
    serde_json::to_string(&payload).map_err(json_to_io)
}

pub(crate) fn checkpoint_from_json(raw: &str) -> Result<Checkpoint> {
    let payload = serde_json::from_str::<Value>(raw).map_err(json_to_io)?;
    let object = payload.as_object().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            "checkpoint payload must be an object",
        )
    })?;
    Ok(Checkpoint {
        task_id: object
            .get("task_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        cycle_index: object
            .get("cycle_index")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or_default(),
        status: checkpoint_status_from_value(
            object
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("running"),
        )?,
        messages: object
            .get("messages")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(Message::from_dict)
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|error| Error::new(ErrorKind::InvalidData, error))
            })
            .transpose()?
            .unwrap_or_default(),
        cycles: object
            .get("cycles")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(CycleRecord::from_dict)
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|error| Error::new(ErrorKind::InvalidData, error))
            })
            .transpose()?
            .unwrap_or_default(),
        shared_state: object
            .get("shared_state")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect(),
    })
}

fn json_to_io(error: serde_json::Error) -> Error {
    Error::new(ErrorKind::InvalidData, error)
}
