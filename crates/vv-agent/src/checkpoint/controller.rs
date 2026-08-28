//! v8 durable controller-command and host-interaction wires.
//!
//! The types in this module deliberately use hand-written codecs.  Controller
//! messages cross process and language boundaries, so serde's permissive
//! defaults are not sufficient: every discriminator, field, fence, digest,
//! and UTF-8 bound is checked before a store is allowed to mutate state.

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use super::{
    canonical_json_bytes, validate_sha256, CheckpointError, CheckpointResult, MAX_WIRE_INTEGER,
};

pub const HOST_INTERACTION_REQUEST_SCHEMA: &str = "vv-agent.host-interaction-request.v1";
pub const HOST_INTERACTION_RESPONSE_SCHEMA: &str = "vv-agent.host-interaction-response.v1";
pub const HOST_INTERACTION_OUTCOME_SCHEMA: &str = "vv-agent.host-interaction-outcome.v1";
pub const HOST_INTERACTION_RECORD_SCHEMA: &str = "vv-agent.host-interaction-record.v1";
pub const HOST_INTERACTION_RECOVERY_SCHEMA: &str = "vv-agent.host-interaction-recovery.v1";
pub const HOST_INTERACTION_RECOVERY_RESULT_SCHEMA: &str =
    "vv-agent.host-interaction-recovery-result.v1";
pub const HOST_INTERACTION_NOTIFICATION_SCHEMA: &str = "vv-agent.host-interaction-notification.v1";
pub const CONTROLLER_COMMAND_SCHEMA: &str = "vv-agent.controller-command.v1";
pub const CONTROLLER_COMMAND_RECEIPT_SCHEMA: &str = "vv-agent.controller-command-receipt.v1";
pub const CONTROLLER_COMMAND_RESOLUTION_SCHEMA: &str = "vv-agent.controller-command-resolution.v1";
pub const CONTROLLER_COMMAND_MAX_UTF8_BYTES: usize = 512;
pub const HOST_INTERACTION_MAX_UTF8_BYTES: usize = 512;
pub const HOST_INTERACTION_CONTENT_MAX_UTF8_BYTES: usize = 65_536;

/// Apply the canonical public host-text policy before a value is hashed or
/// persisted. Locators are removed first so query parameters cannot leak
/// around credential masking, then credential-shaped values are replaced.
pub(crate) fn sanitize_host_text(text: &str) -> String {
    let locator =
        regex::Regex::new(r"(?i)https?://[^\s]+").expect("host locator sanitizer regex is valid");
    let credential = regex::Regex::new(
        r"(?i)\bBearer\s+[A-Za-z0-9._~+/=-]+|\b(?:sk|pk)[-_][A-Za-z0-9._~-]+|\b(?:token|secret|password|api[_-]?key|authorization)\s*(?:[:=]|\s+)\s*(?:bearer\s+)?[A-Za-z0-9._~+/=-]+",
    )
    .expect("host credential sanitizer regex is valid");
    let sanitized = locator.replace_all(text, "[external locator redacted]");
    credential
        .replace_all(&sanitized, "[credential redacted]")
        .into_owned()
}

/// Derive the App Server command identity without trusting a client-supplied
/// command id.  The framing is part of the v8 contract and intentionally
/// differs from a plain JSON digest to keep domain separation explicit.
pub fn derive_controller_command_id(
    thread_id: &str,
    turn_id: &str,
    action_id: &str,
) -> CheckpointResult<String> {
    for (name, value) in [
        ("threadId", thread_id),
        ("turnId", turn_id),
        ("actionId", action_id),
    ] {
        if value.trim().is_empty() || value.len() > CONTROLLER_COMMAND_MAX_UTF8_BYTES {
            return Err(error(
                "controller_command_invalid_state",
                format!("{name} must be non-empty and at most {CONTROLLER_COMMAND_MAX_UTF8_BYTES} UTF-8 bytes"),
            ));
        }
    }
    let payload = serde_json::json!({
        "action_id": action_id,
        "schema_version": "vv-agent.controller-command-id.v1",
        "thread_id": thread_id,
        "turn_id": turn_id,
    });
    let bytes = canonical_json_bytes(&payload, "controller command id")?;
    let mut framed = Vec::with_capacity(38 + bytes.len());
    framed.extend_from_slice(b"vv-agent.controller-command-id.v1");
    framed.push(0);
    framed.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    framed.extend_from_slice(&bytes);
    Ok(format!("{:x}", Sha256::digest(framed)))
}

/// Stable identity for the independent controller recovery-wake outbox row.
/// This is storage metadata, not part of the public receipt wire, but it is
/// deliberately derived from the same canonical inputs in every language.
pub fn controller_receipt_outbox_id(
    command_id: &str,
    command_digest: &str,
) -> CheckpointResult<String> {
    canonical_digest(
        serde_json::json!({
            "command_digest": command_digest,
            "command_id": command_id,
            "schema_version": CONTROLLER_COMMAND_RECEIPT_SCHEMA,
        }),
        "controller receipt outbox id",
    )
}

fn error(code: &str, message: impl Into<String>) -> CheckpointError {
    CheckpointError::new(code, message)
}

fn require_exact_fields(
    object: &Map<String, Value>,
    expected: &[&str],
    code: &str,
) -> CheckpointResult<()> {
    if object.len() != expected.len() || expected.iter().any(|field| !object.contains_key(*field)) {
        let unknown = object
            .keys()
            .filter(|field| !expected.contains(&field.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        let detail = if unknown.is_empty() {
            "required field is missing".to_string()
        } else {
            format!("unknown field(s): {}", unknown.join(", "))
        };
        return Err(error(code, detail));
    }
    Ok(())
}

fn require_fields_with_optional(
    object: &Map<String, Value>,
    required: &[&str],
    optional: &[&str],
    code: &str,
) -> CheckpointResult<()> {
    if required.iter().any(|field| !object.contains_key(*field))
        || object
            .keys()
            .any(|field| !required.contains(&field.as_str()) && !optional.contains(&field.as_str()))
    {
        let unknown = object
            .keys()
            .filter(|field| {
                !required.contains(&field.as_str()) && !optional.contains(&field.as_str())
            })
            .cloned()
            .collect::<Vec<_>>();
        let detail = if unknown.is_empty() {
            "required field is missing".to_string()
        } else {
            format!("unknown field(s): {}", unknown.join(", "))
        };
        return Err(error(code, detail));
    }
    Ok(())
}

fn optional_string(
    object: &Map<String, Value>,
    field: &str,
    max_bytes: usize,
    code: &str,
) -> CheckpointResult<Option<String>> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let value = value
                .as_str()
                .ok_or_else(|| error(code, format!("{field} must be a string or null")))?;
            if value.len() > max_bytes {
                return Err(error(
                    code,
                    format!("{field} exceeds {max_bytes} UTF-8 bytes"),
                ));
            }
            Ok(Some(value.to_string()))
        }
    }
}

fn optional_integer(
    object: &Map<String, Value>,
    field: &str,
    positive: bool,
    code: &str,
) -> CheckpointResult<Option<u64>> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let value = value
                .as_u64()
                .ok_or_else(|| error(code, format!("{field} must be an integer or null")))?;
            if value > MAX_WIRE_INTEGER || (positive && value == 0) {
                return Err(error(
                    code,
                    format!("{field} is outside the JSON-safe integer range"),
                ));
            }
            Ok(Some(value))
        }
    }
}

fn required_string<'a>(
    object: &'a Map<String, Value>,
    field: &str,
    code: &str,
) -> CheckpointResult<&'a str> {
    object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| error(code, format!("{field} must be a string")))
}

fn required_non_empty_string<'a>(
    object: &'a Map<String, Value>,
    field: &str,
    max_bytes: usize,
    code: &str,
) -> CheckpointResult<&'a str> {
    let value = required_string(object, field, code)?;
    if value.trim().is_empty() {
        return Err(error(code, format!("{field} must be non-empty")));
    }
    if value.len() > max_bytes {
        return Err(error(
            code,
            format!("{field} exceeds {max_bytes} UTF-8 bytes"),
        ));
    }
    Ok(value)
}

fn required_integer(
    object: &Map<String, Value>,
    field: &str,
    positive: bool,
) -> CheckpointResult<u64> {
    let value = object.get(field).and_then(Value::as_u64).ok_or_else(|| {
        error(
            "host_interaction_fields_invalid",
            format!("{field} must be an integer"),
        )
    })?;
    if value > MAX_WIRE_INTEGER || (positive && value == 0) {
        return Err(error(
            "host_interaction_fields_invalid",
            format!("{field} is outside the JSON-safe integer range"),
        ));
    }
    Ok(value)
}

fn required_digest(object: &Map<String, Value>, field: &str) -> CheckpointResult<String> {
    let value = required_non_empty_string(object, field, 64, "host_interaction_fields_invalid")?;
    validate_sha256(value, field).map_err(|_| {
        error(
            "host_interaction_fields_invalid",
            format!("{field} must be lowercase SHA-256"),
        )
    })?;
    Ok(value.to_string())
}

fn canonical_digest(mut value: Value, digest_field: &str) -> CheckpointResult<String> {
    let object = value.as_object_mut().ok_or_else(|| {
        error(
            "host_interaction_fields_invalid",
            "wire value must be an object",
        )
    })?;
    object.remove(digest_field);
    let bytes = canonical_json_bytes(&value, "controller wire")?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn value_object(value: Value, code: &str) -> CheckpointResult<Map<String, Value>> {
    value
        .as_object()
        .cloned()
        .ok_or_else(|| error(code, "wire value must be an object"))
}

include!("controller_host.rs");
include!("controller_command.rs");
include!("controller_recovery.rs");
