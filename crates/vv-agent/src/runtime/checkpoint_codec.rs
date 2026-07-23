//! Strict codec for the current checkpoint wire format.

use std::collections::BTreeMap;

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;
use serde_json::{Map, Number, Value};

use crate::budget::BudgetUsageSnapshot;
use crate::checkpoint::{
    canonical_json_bytes, CheckpointError, CheckpointResult, CheckpointStatus, CHECKPOINT_SCHEMA,
    RUN_DEFINITION_SCHEMA,
};
use crate::runtime::state::{
    validate_checkpoint, validate_extension_state_size, Checkpoint, EventOutboxEntry,
    ExtensionStateEntry, OperationJournalEntry,
};
use crate::types::{CycleRecord, Message, ModelCallRecord};

const KNOWN_FIELDS: &[&str] = &[
    "schema_version",
    "run_definition_schema",
    "run_definition",
    "checkpoint_key",
    "task_id",
    "root_run_id",
    "trace_id",
    "run_definition_digest",
    "resume_attempt",
    "cycle_index",
    "status",
    "messages",
    "cycles",
    "model_calls",
    "shared_state",
    "budget_usage",
    "event_cursor",
    "event_outbox",
    "extension_state",
    "model_call_journal",
    "tool_journal",
    "revision",
    "claim_token",
    "claimed_cycle",
    "lease_expires_at_ms",
    "terminal_result",
    "terminal_acknowledged",
];

pub fn checkpoint_to_value(
    checkpoint: &Checkpoint,
    max_extension_state_bytes: u64,
) -> CheckpointResult<Value> {
    validate_checkpoint(checkpoint)?;
    let extension_bytes =
        validate_extension_state_size(&checkpoint.extension_state, max_extension_state_bytes);
    extension_bytes?;

    let mut object = Map::new();
    object.insert(
        "schema_version".to_string(),
        Value::String(CHECKPOINT_SCHEMA.to_string()),
    );
    object.insert(
        "run_definition_schema".to_string(),
        Value::String(RUN_DEFINITION_SCHEMA.to_string()),
    );
    object.insert(
        "run_definition".to_string(),
        checkpoint.run_definition.clone(),
    );
    object.insert(
        "checkpoint_key".to_string(),
        Value::String(checkpoint.checkpoint_key.clone()),
    );
    object.insert(
        "task_id".to_string(),
        Value::String(checkpoint.task_id.clone()),
    );
    object.insert(
        "root_run_id".to_string(),
        Value::String(checkpoint.root_run_id.clone()),
    );
    object.insert(
        "trace_id".to_string(),
        Value::String(checkpoint.trace_id.clone()),
    );
    object.insert(
        "run_definition_digest".to_string(),
        Value::String(checkpoint.run_definition_digest.clone()),
    );
    object.insert(
        "resume_attempt".to_string(),
        Value::from(checkpoint.resume_attempt),
    );
    object.insert(
        "cycle_index".to_string(),
        Value::from(checkpoint.cycle_index),
    );
    object.insert(
        "status".to_string(),
        Value::String(checkpoint.status.as_str().to_string()),
    );
    object.insert(
        "messages".to_string(),
        Value::Array(checkpoint.messages.iter().map(Message::to_dict).collect()),
    );
    object.insert(
        "cycles".to_string(),
        Value::Array(checkpoint.cycles.iter().map(CycleRecord::to_dict).collect()),
    );
    object.insert(
        "model_calls".to_string(),
        serde_json::to_value(&checkpoint.model_calls).map_err(|error| {
            CheckpointError::new("checkpoint_model_calls_invalid", error.to_string())
        })?,
    );
    object.insert(
        "shared_state".to_string(),
        Value::Object(checkpoint.shared_state.clone().into_iter().collect()),
    );
    object.insert(
        "budget_usage".to_string(),
        checkpoint
            .budget_usage
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|error| CheckpointError::new("checkpoint_json_invalid", error.to_string()))?
            .unwrap_or(Value::Null),
    );
    object.insert(
        "event_cursor".to_string(),
        checkpoint
            .event_cursor
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|error| CheckpointError::new("checkpoint_json_invalid", error.to_string()))?
            .unwrap_or(Value::Null),
    );
    object.insert(
        "event_outbox".to_string(),
        Value::Array(
            checkpoint
                .event_outbox
                .iter()
                .map(EventOutboxEntry::to_value)
                .collect(),
        ),
    );
    object.insert(
        "extension_state".to_string(),
        Value::Object(
            checkpoint
                .extension_state
                .iter()
                .map(|(namespace, entry)| (namespace.clone(), entry.to_value()))
                .collect(),
        ),
    );
    object.insert(
        "model_call_journal".to_string(),
        Value::Array(
            checkpoint
                .model_call_journal
                .iter()
                .map(OperationJournalEntry::to_value)
                .collect(),
        ),
    );
    object.insert(
        "tool_journal".to_string(),
        Value::Array(
            checkpoint
                .tool_journal
                .iter()
                .map(OperationJournalEntry::to_value)
                .collect(),
        ),
    );
    object.insert("revision".to_string(), Value::from(checkpoint.revision));
    object.insert(
        "claim_token".to_string(),
        checkpoint
            .claim_token
            .clone()
            .map_or(Value::Null, Value::String),
    );
    object.insert(
        "claimed_cycle".to_string(),
        checkpoint.claimed_cycle.map_or(Value::Null, Value::from),
    );
    object.insert(
        "lease_expires_at_ms".to_string(),
        checkpoint
            .lease_expires_at_ms
            .map_or(Value::Null, Value::from),
    );
    object.insert(
        "terminal_result".to_string(),
        checkpoint.terminal_result.clone().unwrap_or(Value::Null),
    );
    object.insert(
        "terminal_acknowledged".to_string(),
        Value::Bool(checkpoint.terminal_acknowledged),
    );
    Ok(Value::Object(object))
}

pub fn checkpoint_from_value(
    payload: &Value,
    max_extension_state_bytes: u64,
) -> CheckpointResult<Checkpoint> {
    let object = payload.as_object().ok_or_else(|| {
        CheckpointError::new(
            "checkpoint_payload_invalid",
            "checkpoint v3 payload must be an object",
        )
    })?;
    if let Some(field) = object
        .keys()
        .find(|field| !KNOWN_FIELDS.contains(&field.as_str()))
    {
        return Err(CheckpointError::new(
            "checkpoint_unknown_field",
            format!("checkpoint contains unknown field: {field}"),
        ));
    }
    if object.get("schema_version").and_then(Value::as_str) != Some(CHECKPOINT_SCHEMA) {
        return Err(CheckpointError::new(
            "checkpoint_schema_unsupported",
            "checkpoint schema_version is not vv-agent.checkpoint.v3",
        ));
    }
    let run_definition_schema = required_string(
        object,
        "run_definition_schema",
        "checkpoint_definition_schema_unsupported",
    )?;
    if run_definition_schema != RUN_DEFINITION_SCHEMA {
        return Err(CheckpointError::new(
            "checkpoint_definition_schema_unsupported",
            "run_definition_schema is unsupported",
        ));
    }
    let run_definition = object.get("run_definition").cloned().ok_or_else(|| {
        CheckpointError::new(
            "checkpoint_definition_invalid",
            "run_definition is required",
        )
    })?;
    if let Some(field) = KNOWN_FIELDS
        .iter()
        .find(|field| !object.contains_key(**field))
    {
        return Err(CheckpointError::new(
            "checkpoint_field_invalid",
            format!("required checkpoint field {field} is missing"),
        ));
    }
    let checkpoint = Checkpoint {
        schema_version: CHECKPOINT_SCHEMA.to_string(),
        run_definition_schema: run_definition_schema.to_string(),
        run_definition,
        checkpoint_key: required_string(object, "checkpoint_key", "checkpoint_key_invalid")?
            .to_string(),
        task_id: required_string(object, "task_id", "checkpoint_value_invalid")?.to_string(),
        root_run_id: required_string(object, "root_run_id", "checkpoint_value_invalid")?
            .to_string(),
        trace_id: required_string(object, "trace_id", "checkpoint_value_invalid")?.to_string(),
        run_definition_digest: required_string(
            object,
            "run_definition_digest",
            "checkpoint_digest_invalid",
        )?
        .to_string(),
        resume_attempt: required_u64(
            object,
            "resume_attempt",
            "checkpoint_resume_attempt_invalid",
        )?,
        cycle_index: required_u64(object, "cycle_index", "checkpoint_cycle_invalid")?,
        status: parse_status(object, "status")?,
        messages: parse_messages(object.get("messages"))?,
        cycles: parse_cycles(object.get("cycles"))?,
        model_calls: parse_model_calls(object.get("model_calls"))?,
        shared_state: parse_object_map(object, "shared_state", "checkpoint_shared_state_invalid")?,
        budget_usage: parse_optional_budget(object.get("budget_usage"))?,
        event_cursor: parse_optional(object.get("event_cursor"), "event_cursor")?,
        event_outbox: parse_array(object, "event_outbox")?
            .iter()
            .map(EventOutboxEntry::from_value)
            .collect::<CheckpointResult<Vec<_>>>()?,
        extension_state: parse_extensions(object.get("extension_state"))?,
        model_call_journal: parse_array(object, "model_call_journal")?
            .iter()
            .map(OperationJournalEntry::from_value)
            .collect::<CheckpointResult<Vec<_>>>()?,
        tool_journal: parse_array(object, "tool_journal")?
            .iter()
            .map(OperationJournalEntry::from_value)
            .collect::<CheckpointResult<Vec<_>>>()?,
        revision: required_u64(object, "revision", "checkpoint_revision_invalid")?,
        claim_token: parse_optional_string(object, "claim_token")?,
        claimed_cycle: parse_optional_u64(object, "claimed_cycle")?,
        lease_expires_at_ms: parse_optional_u64(object, "lease_expires_at_ms")?,
        terminal_result: parse_optional_value(object, "terminal_result")?,
        terminal_acknowledged: object
            .get("terminal_acknowledged")
            .and_then(Value::as_bool)
            .ok_or_else(|| {
                CheckpointError::new(
                    "checkpoint_status_invalid",
                    "terminal_acknowledged must be a boolean",
                )
            })?,
    };
    validate_checkpoint(&checkpoint)?;
    crate::runtime::state::validate_extension_state_size(
        &checkpoint.extension_state,
        max_extension_state_bytes,
    )?;
    Ok(checkpoint)
}

pub fn checkpoint_to_json(
    checkpoint: &Checkpoint,
    max_extension_state_bytes: u64,
) -> CheckpointResult<String> {
    let value = checkpoint_to_value(checkpoint, max_extension_state_bytes)?;
    let bytes = canonical_json_bytes(&value, "checkpoint v3")?;
    String::from_utf8(bytes).map_err(|error| {
        CheckpointError::new(
            "checkpoint_canonicalization_invalid",
            format!("checkpoint canonical JSON is not UTF-8: {error}"),
        )
    })
}

pub fn checkpoint_from_json(
    payload: &str,
    max_extension_state_bytes: u64,
) -> CheckpointResult<Checkpoint> {
    let value = strict_json_value(payload)?;
    checkpoint_from_value(&value, max_extension_state_bytes)
}

fn parse_status(object: &Map<String, Value>, field: &str) -> CheckpointResult<CheckpointStatus> {
    let value = required_string(object, field, "checkpoint_status_invalid")?;
    serde_json::from_value(Value::String(value.to_string())).map_err(|_| {
        CheckpointError::new(
            "checkpoint_status_invalid",
            format!("unknown checkpoint status {value}"),
        )
    })
}

fn parse_messages(value: Option<&Value>) -> CheckpointResult<Vec<Message>> {
    let values = value.and_then(Value::as_array).ok_or_else(|| {
        CheckpointError::new("checkpoint_messages_invalid", "messages must be an array")
    })?;
    values
        .iter()
        .map(|value| {
            Message::from_dict(value)
                .map_err(|error| CheckpointError::new("checkpoint_messages_invalid", error))
        })
        .collect()
}

fn parse_cycles(value: Option<&Value>) -> CheckpointResult<Vec<CycleRecord>> {
    let values = value.and_then(Value::as_array).ok_or_else(|| {
        CheckpointError::new("checkpoint_cycles_invalid", "cycles must be an array")
    })?;
    values
        .iter()
        .map(|value| {
            CycleRecord::from_dict(value)
                .map_err(|error| CheckpointError::new("checkpoint_cycles_invalid", error))
        })
        .collect()
}

fn parse_model_calls(value: Option<&Value>) -> CheckpointResult<Vec<ModelCallRecord>> {
    let values = value.and_then(Value::as_array).ok_or_else(|| {
        CheckpointError::new(
            "checkpoint_model_calls_invalid",
            "model_calls must be an array",
        )
    })?;
    values
        .iter()
        .cloned()
        .map(|value| {
            serde_json::from_value(value).map_err(|error| {
                CheckpointError::new("checkpoint_model_calls_invalid", error.to_string())
            })
        })
        .collect()
}

fn parse_object_map(
    object: &Map<String, Value>,
    field: &str,
    code: &str,
) -> CheckpointResult<BTreeMap<String, Value>> {
    object
        .get(field)
        .and_then(Value::as_object)
        .cloned()
        .map(|map| map.into_iter().collect())
        .ok_or_else(|| CheckpointError::new(code, format!("{field} must be an object")))
}

fn parse_optional_budget(value: Option<&Value>) -> CheckpointResult<Option<BudgetUsageSnapshot>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => serde_json::from_value(value.clone())
            .map(Some)
            .map_err(|error| {
                CheckpointError::new("checkpoint_budget_usage_invalid", error.to_string())
            }),
    }
}

fn parse_optional<T>(value: Option<&Value>, field: &str) -> CheckpointResult<Option<T>>
where
    T: serde::de::DeserializeOwned,
{
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => serde_json::from_value(value.clone())
            .map(Some)
            .map_err(|error| {
                CheckpointError::new("checkpoint_field_invalid", format!("{field}: {error}"))
            }),
    }
}

fn parse_extensions(
    value: Option<&Value>,
) -> CheckpointResult<BTreeMap<String, ExtensionStateEntry>> {
    let object = value.and_then(Value::as_object).ok_or_else(|| {
        CheckpointError::new(
            "checkpoint_extension_state_invalid",
            "extension_state must be an object",
        )
    })?;
    object
        .iter()
        .map(|(namespace, value)| Ok((namespace.clone(), ExtensionStateEntry::from_value(value)?)))
        .collect()
}

fn parse_optional_string(
    object: &Map<String, Value>,
    field: &str,
) -> CheckpointResult<Option<String>> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(CheckpointError::new(
            "checkpoint_claim_invalid",
            format!("{field} must be a string or null"),
        )),
    }
}

fn parse_optional_u64(object: &Map<String, Value>, field: &str) -> CheckpointResult<Option<u64>> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => number.as_u64().map(Some).ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_integer_invalid",
                format!("{field} must be a non-negative integer"),
            )
        }),
        Some(_) => Err(CheckpointError::new(
            "checkpoint_integer_invalid",
            format!("{field} must be an integer or null"),
        )),
    }
}

fn parse_optional_value(
    object: &Map<String, Value>,
    field: &str,
) -> CheckpointResult<Option<Value>> {
    Ok(object.get(field).filter(|value| !value.is_null()).cloned())
}

fn parse_array<'a>(
    object: &'a Map<String, Value>,
    field: &str,
) -> CheckpointResult<&'a Vec<Value>> {
    object.get(field).and_then(Value::as_array).ok_or_else(|| {
        CheckpointError::new(
            "checkpoint_field_invalid",
            format!("{field} must be an array"),
        )
    })
}

fn required_string<'a>(
    object: &'a Map<String, Value>,
    field: &str,
    code: &str,
) -> CheckpointResult<&'a str> {
    object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| CheckpointError::new(code, format!("{field} must be a string")))
}

fn required_u64(object: &Map<String, Value>, field: &str, code: &str) -> CheckpointResult<u64> {
    object.get(field).and_then(Value::as_u64).ok_or_else(|| {
        CheckpointError::new(code, format!("{field} must be a non-negative integer"))
    })
}

fn strict_json_value(payload: &str) -> CheckpointResult<Value> {
    let mut deserializer = serde_json::Deserializer::from_str(payload);
    let StrictValue(value) = StrictValue::deserialize(&mut deserializer)
        .map_err(|error| CheckpointError::new("checkpoint_json_invalid", error.to_string()))?;
    deserializer
        .end()
        .map_err(|error| CheckpointError::new("checkpoint_json_invalid", error.to_string()))?;
    Ok(value)
}

struct StrictValue(Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictValueVisitor)
    }
}

struct StrictValueVisitor;

impl<'de> Visitor<'de> for StrictValueVisitor {
    type Value = StrictValue;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(Number::from(value))))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(Number::from(value))))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Number::from_f64(value)
            .map(Value::Number)
            .map(StrictValue)
            .ok_or_else(|| E::custom("non-finite JSON number"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::String(value.to_string())))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::String(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::with_capacity(sequence.size_hint().unwrap_or(0));
        while let Some(StrictValue(value)) = sequence.next_element()? {
            values.push(value);
        }
        Ok(StrictValue(Value::Array(values)))
    }

    fn visit_map<A>(self, mut object: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = Map::new();
        while let Some(key) = object.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(de::Error::custom(format!(
                    "duplicate JSON object key {key}"
                )));
            }
            let StrictValue(value) = object.next_value()?;
            values.insert(key, value);
        }
        Ok(StrictValue(Value::Object(values)))
    }
}
