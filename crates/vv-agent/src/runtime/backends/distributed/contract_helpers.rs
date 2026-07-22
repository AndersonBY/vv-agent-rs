use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::budget::MAX_WIRE_INTEGER;
use crate::checkpoint::RUN_DEFINITION_SCHEMA;

pub(crate) fn validate_current_discriminator_fields(payload: &Value) -> Result<(), String> {
    if payload.get("run_definition_schema").and_then(Value::as_str) != Some(RUN_DEFINITION_SCHEMA) {
        return Err("checkpoint_definition_schema_unsupported".to_string());
    }
    if payload.get("checkpoint_config").is_none_or(Value::is_null) {
        return Err("distributed run requires checkpoint_config".to_string());
    }
    if !matches!(
        payload.get("claim_mode").and_then(Value::as_str),
        Some("continue" | "recovery")
    ) {
        return Err("checkpoint_claim_mode_invalid".to_string());
    }
    Ok(())
}

pub(crate) fn normalize_integral_float(object: &mut serde_json::Map<String, Value>, field: &str) {
    let Some(value) = object.get(field).and_then(Value::as_f64) else {
        return;
    };
    if value.is_finite() && value >= 0.0 && value.fract() == 0.0 && value <= MAX_WIRE_INTEGER as f64
    {
        object.insert(field.to_string(), Value::from(value as u64));
    }
}

pub(crate) fn now_unix_ms() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_millis()
        .try_into()
        .map_err(|_| "system clock milliseconds exceed u64".to_string())
}

pub(crate) fn require_non_empty(value: &str, field_name: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        Err(format!("{field_name} must be a non-empty string"))
    } else {
        Ok(())
    }
}

pub(crate) fn validate_sorted_unique(
    values: &[String],
    field_name: &str,
    validate: impl Fn(&str) -> Result<(), String>,
) -> Result<(), String> {
    for value in values {
        validate(value)?;
    }
    if values.windows(2).any(|window| window[0] >= window[1]) {
        return Err(format!("{field_name} must be sorted and unique"));
    }
    Ok(())
}

pub(crate) fn validate_json_pointer(pointer: &str) -> Result<(), String> {
    if pointer.is_empty() {
        return Ok(());
    }
    if !pointer.starts_with('/') {
        return Err("credential_slots must contain RFC 6901 JSON pointers".to_string());
    }
    let bytes = pointer.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'~' {
            let Some(next) = bytes.get(index + 1) else {
                return Err("credential_slots must contain RFC 6901 JSON pointers".to_string());
            };
            if !matches!(*next, b'0' | b'1') {
                return Err("credential_slots must contain RFC 6901 JSON pointers".to_string());
            }
            index += 1;
        }
        index += 1;
    }
    Ok(())
}

pub(crate) fn utf16_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}
