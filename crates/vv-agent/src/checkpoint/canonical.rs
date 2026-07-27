use super::*;

mod json_pointer;
mod run_definition;

pub(super) use json_pointer::validate_pointer;
pub use json_pointer::{resolve_json_pointer, set_json_pointer};
pub use run_definition::{
    canonical_run_definition_bytes, normalize_run_definition, redact_run_definition,
    run_definition_digest, validate_run_definition,
};

pub fn event_payload_digest(event: &Value) -> CheckpointResult<String> {
    if !event.is_object() {
        return Err(CheckpointError::new(
            "event_payload_invalid",
            "event payload must be an object",
        ));
    }
    sha256_canonical(event, "event payload")
}

pub fn model_request_digest(request: &Value) -> CheckpointResult<String> {
    operation_request_digest(OperationKind::Model, request)
}

pub fn tool_request_digest(
    tool_call_id: &str,
    tool_name: &str,
    arguments: &Value,
    idempotency_key: Option<&str>,
) -> CheckpointResult<String> {
    require_non_empty(tool_call_id, "tool_call_id")?;
    require_non_empty(tool_name, "tool_name")?;
    if idempotency_key.is_some_and(str::is_empty) {
        return Err(CheckpointError::new(
            "operation_request_invalid",
            "idempotency_key must be null or non-empty",
        ));
    }
    if !arguments.is_object() {
        return Err(CheckpointError::new(
            "operation_request_invalid",
            "tool arguments must be an object",
        ));
    }
    operation_request_digest(
        OperationKind::Tool,
        &serde_json::json!({
            "schema_version": OPERATION_REQUEST_SCHEMA,
            "kind": "tool",
            "request": {
            "tool_call_id": tool_call_id,
            "tool_name": tool_name,
            "arguments": arguments,
            "idempotency_key": idempotency_key,
            },
        }),
    )
}

pub fn operation_request_digest(kind: OperationKind, request: &Value) -> CheckpointResult<String> {
    let object = request.as_object().ok_or_else(|| {
        CheckpointError::new(
            "operation_request_invalid",
            "operation request must be an object",
        )
    })?;
    let expected = ["schema_version", "kind", "request"];
    if object.len() != expected.len() || expected.iter().any(|field| !object.contains_key(*field)) {
        return Err(CheckpointError::new(
            "operation_request_invalid",
            "operation request has missing or unknown fields",
        ));
    }
    if object.get("schema_version").and_then(Value::as_str) != Some(OPERATION_REQUEST_SCHEMA) {
        return Err(CheckpointError::new(
            "operation_request_schema_unsupported",
            "operation request schema is unsupported",
        ));
    }
    let actual_kind = object.get("kind").and_then(Value::as_str).ok_or_else(|| {
        CheckpointError::new(
            "operation_request_invalid",
            "operation request kind is invalid",
        )
    })?;
    let expected_kind = match kind {
        OperationKind::Model => "model",
        OperationKind::Tool => "tool",
    };
    if actual_kind != expected_kind {
        return Err(CheckpointError::new(
            "operation_request_invalid",
            "operation request kind does not match the journal operation",
        ));
    }
    let request_payload = object
        .get("request")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            CheckpointError::new(
                "operation_request_invalid",
                "operation request payload must be an object",
            )
        })?;
    let required: &[&str] = match kind {
        OperationKind::Model => &[
            "model",
            "messages",
            "settings",
            "tools",
            "output_schema",
            "idempotency_key",
        ],
        OperationKind::Tool => &["tool_call_id", "tool_name", "arguments", "idempotency_key"],
    };
    if request_payload.len() != required.len()
        || required
            .iter()
            .any(|field| !request_payload.contains_key(*field))
    {
        return Err(CheckpointError::new(
            "operation_request_invalid",
            "operation request payload has missing or unknown fields",
        ));
    }
    validate_i_json(request, "operation request")
        .map_err(|error| CheckpointError::new("operation_request_not_i_json", error.message))?;
    sha256_canonical(request, "operation request")
}

pub fn canonical_json_bytes(value: &Value, field_name: &str) -> CheckpointResult<Vec<u8>> {
    validate_i_json(value, field_name)?;
    serde_json_canonicalizer::to_vec(value).map_err(|error| {
        CheckpointError::new(
            "checkpoint_canonicalization_invalid",
            format!("{field_name} cannot be canonicalized: {error}"),
        )
    })
}

pub fn validate_extension_namespace(namespace: &str) -> CheckpointResult<()> {
    if namespace.is_empty() || !namespace.is_ascii() {
        return Err(CheckpointError::new(
            "checkpoint_extension_namespace_invalid",
            "extension namespace must be non-empty ASCII",
        ));
    }
    if namespace.len() > MAX_EXTENSION_NAMESPACE_BYTES {
        return Err(CheckpointError::new(
            "checkpoint_extension_namespace_invalid",
            format!("extension namespace exceeds {MAX_EXTENSION_NAMESPACE_BYTES} bytes"),
        ));
    }
    let Some(first) = namespace.as_bytes().first().copied() else {
        unreachable!("empty namespace handled above");
    };
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return Err(CheckpointError::new(
            "checkpoint_extension_namespace_invalid",
            "extension namespace must begin with a lowercase letter or digit",
        ));
    }
    if !namespace.contains('.')
        || namespace.bytes().any(|byte| {
            !(byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte))
        })
    {
        return Err(CheckpointError::new(
            "checkpoint_extension_namespace_invalid",
            "extension namespace does not match the reverse-DNS grammar",
        ));
    }
    Ok(())
}

pub fn validate_sha256(value: &str, field_name: &str) -> CheckpointResult<()> {
    if value.len() != 64
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_hexdigit() || byte.is_ascii_uppercase())
    {
        return Err(CheckpointError::new(
            "checkpoint_digest_invalid",
            format!("{field_name} must be a lowercase SHA-256 hex digest"),
        ));
    }
    Ok(())
}

pub fn validate_checkpoint_key(key: &str) -> CheckpointResult<()> {
    if key.trim().is_empty() || key.len() > MAX_CHECKPOINT_KEY_BYTES {
        return Err(CheckpointError::new(
            "checkpoint_key_invalid",
            format!(
                "checkpoint key must be non-empty and at most {MAX_CHECKPOINT_KEY_BYTES} UTF-8 bytes"
            ),
        ));
    }
    Ok(())
}

fn sha256_canonical(value: &Value, field_name: &str) -> CheckpointResult<String> {
    let bytes = canonical_json_bytes(value, field_name)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

pub(super) fn validate_i_json(value: &Value, field_name: &str) -> CheckpointResult<()> {
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) => Ok(()),
        Value::Number(number) => validate_number(number, field_name),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .try_for_each(|(index, item)| validate_i_json(item, &format!("{field_name}[{index}]"))),
        Value::Object(object) => object
            .iter()
            .try_for_each(|(key, item)| validate_i_json(item, &format!("{field_name}.{key}"))),
    }
}

fn validate_number(number: &Number, field_name: &str) -> CheckpointResult<()> {
    if let Some(value) = number.as_u64() {
        if value > MAX_WIRE_INTEGER {
            return Err(CheckpointError::new(
                "checkpoint_definition_not_i_json",
                format!("{field_name} is outside the JSON-safe integer range"),
            ));
        }
    } else if let Some(value) = number.as_i64() {
        if value.unsigned_abs() > MAX_WIRE_INTEGER {
            return Err(CheckpointError::new(
                "checkpoint_definition_not_i_json",
                format!("{field_name} is outside the JSON-safe integer range"),
            ));
        }
    } else if number.as_f64().is_none_or(|value| !value.is_finite()) {
        return Err(CheckpointError::new(
            "checkpoint_definition_not_i_json",
            format!("{field_name} is not a finite JSON number"),
        ));
    }
    Ok(())
}

pub(super) fn validate_capability_ref(
    reference: &CapabilityRef,
    field_name: &str,
) -> CheckpointResult<()> {
    if reference.id.trim().is_empty() || reference.version.trim().is_empty() {
        return Err(CheckpointError::new(
            "checkpoint_capability_ref_invalid",
            format!("{field_name} requires non-empty id and version"),
        ));
    }
    Ok(())
}

pub(super) fn validate_capability_slot(slot: &str) -> CheckpointResult<()> {
    if slot.is_empty()
        || !slot.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        || slot.bytes().any(|byte| {
            !(byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_.:-".contains(&byte))
        })
    {
        return Err(CheckpointError::new(
            "checkpoint_capability_ref_invalid",
            format!("invalid capability reference slot {slot}"),
        ));
    }
    Ok(())
}

pub(super) fn require_non_empty(value: &str, field_name: &str) -> CheckpointResult<()> {
    if value.trim().is_empty() {
        return Err(CheckpointError::new(
            "checkpoint_value_invalid",
            format!("{field_name} must be non-empty"),
        ));
    }
    Ok(())
}

pub(super) fn require_positive(value: u64, field_name: &str) -> CheckpointResult<()> {
    if value == 0 || value > MAX_WIRE_INTEGER {
        return Err(CheckpointError::new(
            "checkpoint_integer_invalid",
            format!("{field_name} must be between 1 and {MAX_WIRE_INTEGER}"),
        ));
    }
    Ok(())
}

pub(super) fn utf16_cmp(left: &str, right: &str) -> Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}
