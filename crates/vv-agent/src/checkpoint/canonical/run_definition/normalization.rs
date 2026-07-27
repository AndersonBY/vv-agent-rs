use std::cmp::Ordering;
use std::collections::BTreeSet;

use serde_json::{Map, Value};

use super::super::{
    set_json_pointer, utf16_cmp, validate_pointer, CheckpointError, CheckpointResult,
    CREDENTIAL_REDACTED,
};
use super::validate_run_definition;

/// Normalize field-specific sets in a definition, lower-case provider header
/// names, redact declared credential slots, and validate the result.
pub fn normalize_run_definition(
    definition: &Value,
    credential_slots: &[String],
) -> CheckpointResult<Value> {
    let mut normalized = definition.clone();
    let object = normalized.as_object_mut().ok_or_else(|| {
        CheckpointError::new(
            "checkpoint_definition_invalid",
            "run_definition must be an object",
        )
    })?;
    let slots = credential_slots
        .iter()
        .cloned()
        .map(Value::String)
        .collect::<Vec<_>>();
    object.insert("credential_slots".to_string(), Value::Array(slots));
    normalize_headers(object.get_mut("model"))?;
    normalize_tool_definitions(object.get_mut("tools"))?;
    normalize_tool_policy(object.get_mut("tool_policy"))?;
    normalize_extensions(object.get_mut("extensions"))?;
    let normalized = redact_run_definition(&Value::Object(object.clone()), credential_slots)?;
    validate_run_definition(&normalized)?;
    Ok(normalized)
}

pub fn redact_run_definition(
    definition: &Value,
    credential_slots: &[String],
) -> CheckpointResult<Value> {
    let mut redacted = definition.clone();
    let mut previous: Option<&str> = None;
    for slot in credential_slots {
        if let Some(previous_slot) = previous {
            if utf16_cmp(previous_slot, slot) != Ordering::Less {
                return Err(CheckpointError::new(
                    "checkpoint_credential_slots_invalid",
                    "credential slots must be sorted and unique",
                ));
            }
        }
        validate_pointer(slot)?;
        set_json_pointer(
            &mut redacted,
            slot,
            Value::String(CREDENTIAL_REDACTED.to_string()),
        )?;
        previous = Some(slot);
    }
    Ok(redacted)
}

pub(super) fn validate_header_names(model: Option<&Value>) -> CheckpointResult<()> {
    let Some(model) = model.and_then(Value::as_object) else {
        return Ok(());
    };
    let Some(settings) = model.get("settings").and_then(Value::as_object) else {
        return Ok(());
    };
    let Some(headers) = settings.get("extra_headers").and_then(Value::as_object) else {
        return Ok(());
    };
    let mut normalized = BTreeSet::new();
    for name in headers.keys() {
        let lower = name.to_ascii_lowercase();
        if !normalized.insert(lower) {
            return Err(CheckpointError::new(
                "checkpoint_definition_header_collision",
                "header names collide after ASCII lowercasing",
            ));
        }
    }
    Ok(())
}

fn normalize_headers(model: Option<&mut Value>) -> CheckpointResult<()> {
    let Some(model) = model.and_then(Value::as_object_mut) else {
        return Ok(());
    };
    let Some(settings) = model.get_mut("settings").and_then(Value::as_object_mut) else {
        return Ok(());
    };
    let Some(headers) = settings
        .get_mut("extra_headers")
        .and_then(Value::as_object_mut)
    else {
        return Ok(());
    };
    let mut normalized = Map::new();
    for (name, value) in std::mem::take(headers) {
        let lower = name.to_ascii_lowercase();
        if normalized.insert(lower, value).is_some() {
            return Err(CheckpointError::new(
                "checkpoint_definition_header_collision",
                "header names collide after ASCII lowercasing",
            ));
        }
    }
    *headers = normalized;
    Ok(())
}

fn normalize_tool_definitions(tools: Option<&mut Value>) -> CheckpointResult<()> {
    let Some(tools) = tools.and_then(Value::as_array_mut) else {
        return Ok(());
    };
    for tool in tools {
        let object = tool.as_object_mut().ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_definition_invalid",
                "run_definition tools must contain objects",
            )
        })?;
        let metadata_value = object.get("tool_metadata").cloned().ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_definition_invalid",
                "run_definition tool metadata is required",
            )
        })?;
        let normalized = if metadata_value.is_null() {
            None
        } else {
            Some(
                serde_json::from_value::<crate::tools::ToolMetadata>(metadata_value).map_err(
                    |error| {
                        CheckpointError::new(
                            "checkpoint_definition_invalid",
                            format!("run_definition tool metadata is invalid: {error}"),
                        )
                    },
                )?,
            )
        };
        object.insert(
            "tool_metadata".to_string(),
            normalized
                .map(|value| serde_json::to_value(value).expect("tool metadata serializes"))
                .unwrap_or(Value::Null),
        );
    }
    Ok(())
}

pub(super) fn normalize_tool_policy(policy: Option<&mut Value>) -> CheckpointResult<()> {
    let Some(policy) = policy.and_then(Value::as_object_mut) else {
        return Ok(());
    };
    for key in ["allowed_tools", "disallowed_tools"] {
        let Some(values) = policy.get_mut(key).and_then(Value::as_array_mut) else {
            continue;
        };
        let mut strings = values
            .iter()
            .map(|value| {
                value.as_str().map(str::to_string).ok_or_else(|| {
                    CheckpointError::new(
                        "checkpoint_definition_invalid",
                        format!("tool policy {key} must contain strings"),
                    )
                })
            })
            .collect::<CheckpointResult<Vec<_>>>()?;
        strings.sort_by(|left, right| utf16_cmp(left, right));
        strings.dedup();
        *values = strings.into_iter().map(Value::String).collect();
    }
    let metadata_fields_present = [
        "denied_side_effects",
        "denied_capability_tags",
        "deny_terminal_tools",
        "denied_cost_dimensions",
    ]
    .map(|field| policy.contains_key(field));
    if metadata_fields_present.iter().any(|present| *present) {
        let denied_side_effects = policy
            .get("denied_side_effects")
            .cloned()
            .map(|value| serde_json::from_value(value).map_err(|_| ()))
            .transpose()
            .map_err(|()| {
                CheckpointError::new(
                    "checkpoint_definition_invalid",
                    "tool policy denied_side_effects is invalid",
                )
            })?
            .unwrap_or_default();
        let denied_capability_tags = policy
            .get("denied_capability_tags")
            .cloned()
            .map(|value| serde_json::from_value(value).map_err(|_| ()))
            .transpose()
            .map_err(|()| {
                CheckpointError::new(
                    "checkpoint_definition_invalid",
                    "tool policy denied_capability_tags is invalid",
                )
            })?
            .unwrap_or_default();
        let deny_terminal_tools = match policy.get("deny_terminal_tools") {
            Some(Value::Bool(value)) => *value,
            Some(_) => {
                return Err(CheckpointError::new(
                    "checkpoint_definition_invalid",
                    "tool policy deny_terminal_tools must be boolean",
                ))
            }
            None => false,
        };
        let denied_cost_dimensions = policy
            .get("denied_cost_dimensions")
            .cloned()
            .map(|value| serde_json::from_value(value).map_err(|_| ()))
            .transpose()
            .map_err(|()| {
                CheckpointError::new(
                    "checkpoint_definition_invalid",
                    "tool policy denied_cost_dimensions is invalid",
                )
            })?
            .unwrap_or_default();
        let normalized = crate::tools::ToolPolicy {
            denied_side_effects,
            denied_capability_tags,
            deny_terminal_tools,
            denied_cost_dimensions,
            ..crate::tools::ToolPolicy::default()
        }
        .normalized()
        .map_err(|error| {
            CheckpointError::new("checkpoint_definition_invalid", error.to_string())
        })?;
        let normalized_values = [
            serde_json::to_value(normalized.denied_side_effects)
                .expect("tool side effects serialize"),
            serde_json::to_value(normalized.denied_capability_tags)
                .expect("tool capability tags serialize"),
            Value::Bool(normalized.deny_terminal_tools),
            serde_json::to_value(normalized.denied_cost_dimensions)
                .expect("tool cost dimensions serialize"),
        ];
        for ((field, present), value) in [
            "denied_side_effects",
            "denied_capability_tags",
            "deny_terminal_tools",
            "denied_cost_dimensions",
        ]
        .into_iter()
        .zip(metadata_fields_present)
        .zip(normalized_values)
        {
            if present {
                policy.insert(field.to_string(), value);
            }
        }
    }
    Ok(())
}

fn normalize_extensions(extensions: Option<&mut Value>) -> CheckpointResult<()> {
    let Some(extensions) = extensions.and_then(Value::as_array_mut) else {
        return Ok(());
    };
    extensions.sort_by(|left, right| {
        left.get("namespace")
            .and_then(Value::as_str)
            .cmp(&right.get("namespace").and_then(Value::as_str))
    });
    Ok(())
}
