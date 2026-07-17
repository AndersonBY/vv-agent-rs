use super::*;

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
    idempotency_key: &str,
) -> CheckpointResult<String> {
    require_non_empty(tool_call_id, "tool_call_id")?;
    require_non_empty(tool_name, "tool_name")?;
    require_non_empty(idempotency_key, "idempotency_key")?;
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

pub fn run_definition_digest(definition: &Value) -> CheckpointResult<String> {
    validate_run_definition(definition)?;
    sha256_canonical(definition, "run_definition")
}

pub fn canonical_run_definition_bytes(definition: &Value) -> CheckpointResult<Vec<u8>> {
    validate_run_definition(definition)?;
    canonical_json_bytes(definition, "run_definition")
}

pub fn validate_run_definition(definition: &Value) -> CheckpointResult<()> {
    validate_i_json(definition, "run_definition")?;
    let object = definition.as_object().ok_or_else(|| {
        CheckpointError::new(
            "checkpoint_definition_invalid",
            "run_definition must be an object",
        )
    })?;
    let required = [
        "schema_version",
        "agent",
        "root_input",
        "compiled_prompt",
        "initial_messages",
        "initial_shared_state",
        "run_metadata",
        "context_ref",
        "model",
        "credential_slots",
        "runtime_controls",
        "tools",
        "tool_policy",
        "checkpoint_policy",
        "budget_limits",
        "output_schema",
        "workspace_ref",
        "session_ref",
        "extensions",
        "capability_refs",
    ];
    let required_set = required.iter().copied().collect::<BTreeSet<_>>();
    if object
        .keys()
        .any(|key| !required_set.contains(key.as_str()))
        || required.iter().any(|key| !object.contains_key(*key))
    {
        return Err(CheckpointError::new(
            "checkpoint_definition_invalid",
            "run_definition has missing or unknown top-level fields",
        ));
    }
    if object.get("schema_version").and_then(Value::as_str) != Some(RUN_DEFINITION_SCHEMA) {
        return Err(CheckpointError::new(
            "checkpoint_definition_schema_unsupported",
            "run_definition schema_version is unsupported",
        ));
    }
    require_object(object.get("agent"), "run_definition.agent")?;
    require_string(object.get("root_input"), "run_definition.root_input")?;
    require_string(
        object.get("compiled_prompt"),
        "run_definition.compiled_prompt",
    )?;
    require_array(
        object.get("initial_messages"),
        "run_definition.initial_messages",
    )?;
    require_object(
        object.get("initial_shared_state"),
        "run_definition.initial_shared_state",
    )?;
    require_object(object.get("run_metadata"), "run_definition.run_metadata")?;
    require_object(object.get("model"), "run_definition.model")?;
    require_object(
        object.get("runtime_controls"),
        "run_definition.runtime_controls",
    )?;
    require_array(object.get("tools"), "run_definition.tools")?;
    require_object(object.get("tool_policy"), "run_definition.tool_policy")?;
    require_object(
        object.get("checkpoint_policy"),
        "run_definition.checkpoint_policy",
    )?;
    require_object(
        object.get("capability_refs"),
        "run_definition.capability_refs",
    )?;

    validate_optional_object_or_null(object.get("context_ref"), "run_definition.context_ref")?;
    validate_optional_object_or_null(object.get("budget_limits"), "run_definition.budget_limits")?;
    validate_optional_object_or_null(object.get("output_schema"), "run_definition.output_schema")?;
    validate_optional_object_or_null(object.get("workspace_ref"), "run_definition.workspace_ref")?;
    validate_optional_object_or_null(object.get("session_ref"), "run_definition.session_ref")?;

    let slots = require_array(
        object.get("credential_slots"),
        "run_definition.credential_slots",
    )?;
    validate_credential_slots(definition, slots)?;
    validate_capability_refs(
        object.get("capability_refs"),
        "run_definition.capability_refs",
    )?;
    validate_extensions(object.get("extensions"), "run_definition.extensions")?;
    validate_tool_policy(object.get("tool_policy"))?;
    validate_checkpoint_policy(object.get("checkpoint_policy"))?;
    validate_header_names(object.get("model"))?;
    Ok(())
}

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

pub fn resolve_json_pointer<'a>(value: &'a Value, pointer: &str) -> CheckpointResult<&'a Value> {
    let tokens = pointer_tokens(pointer)?;
    let mut current = value;
    for token in tokens {
        current = match current {
            Value::Object(object) => object.get(&token).ok_or_else(|| {
                CheckpointError::new(
                    "checkpoint_credential_slot_unresolved",
                    format!("JSON pointer {pointer} does not resolve"),
                )
            })?,
            Value::Array(array) => {
                let index = token.parse::<usize>().map_err(|_| {
                    CheckpointError::new(
                        "checkpoint_credential_slot_unresolved",
                        format!("JSON pointer {pointer} has an invalid array index"),
                    )
                })?;
                array.get(index).ok_or_else(|| {
                    CheckpointError::new(
                        "checkpoint_credential_slot_unresolved",
                        format!("JSON pointer {pointer} does not resolve"),
                    )
                })?
            }
            _ => {
                return Err(CheckpointError::new(
                    "checkpoint_credential_slot_unresolved",
                    format!("JSON pointer {pointer} does not resolve"),
                ));
            }
        };
    }
    Ok(current)
}

pub fn set_json_pointer(
    value: &mut Value,
    pointer: &str,
    replacement: Value,
) -> CheckpointResult<()> {
    let tokens = pointer_tokens(pointer)?;
    if tokens.is_empty() {
        *value = replacement;
        return Ok(());
    }
    let mut current = value;
    for token in &tokens[..tokens.len() - 1] {
        current = match current {
            Value::Object(object) => object.get_mut(token).ok_or_else(|| {
                CheckpointError::new(
                    "checkpoint_credential_slot_unresolved",
                    format!("JSON pointer {pointer} does not resolve"),
                )
            })?,
            Value::Array(array) => {
                let index = token.parse::<usize>().map_err(|_| {
                    CheckpointError::new(
                        "checkpoint_credential_slot_unresolved",
                        format!("JSON pointer {pointer} has an invalid array index"),
                    )
                })?;
                array.get_mut(index).ok_or_else(|| {
                    CheckpointError::new(
                        "checkpoint_credential_slot_unresolved",
                        format!("JSON pointer {pointer} does not resolve"),
                    )
                })?
            }
            _ => {
                return Err(CheckpointError::new(
                    "checkpoint_credential_slot_unresolved",
                    format!("JSON pointer {pointer} does not resolve"),
                ));
            }
        };
    }
    let last = tokens.last().expect("non-empty pointer tokens");
    match current {
        Value::Object(object) => {
            if !object.contains_key(last) {
                return Err(CheckpointError::new(
                    "checkpoint_credential_slot_unresolved",
                    format!("JSON pointer {pointer} does not resolve"),
                ));
            }
            object.insert(last.clone(), replacement);
        }
        Value::Array(array) => {
            let index = last.parse::<usize>().map_err(|_| {
                CheckpointError::new(
                    "checkpoint_credential_slot_unresolved",
                    format!("JSON pointer {pointer} has an invalid array index"),
                )
            })?;
            let Some(item) = array.get_mut(index) else {
                return Err(CheckpointError::new(
                    "checkpoint_credential_slot_unresolved",
                    format!("JSON pointer {pointer} does not resolve"),
                ));
            };
            *item = replacement;
        }
        _ => {
            return Err(CheckpointError::new(
                "checkpoint_credential_slot_unresolved",
                format!("JSON pointer {pointer} does not resolve"),
            ));
        }
    }
    Ok(())
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

fn validate_capability_refs(value: Option<&Value>, field_name: &str) -> CheckpointResult<()> {
    let object = require_object(value, field_name)?;
    for (slot, reference) in object {
        validate_capability_slot(slot)?;
        let reference_object = require_object(Some(reference), &format!("{field_name}.{slot}"))?;
        if reference_object.len() != 2
            || !reference_object.contains_key("id")
            || !reference_object.contains_key("version")
        {
            return Err(CheckpointError::new(
                "checkpoint_capability_ref_invalid",
                format!("{field_name}.{slot} must contain exactly id and version"),
            ));
        }
        require_string(reference_object.get("id"), "capability reference id")?;
        require_string(
            reference_object.get("version"),
            "capability reference version",
        )?;
    }
    Ok(())
}

fn validate_extensions(value: Option<&Value>, field_name: &str) -> CheckpointResult<()> {
    let extensions = require_array(value, field_name)?;
    let mut previous: Option<&str> = None;
    for extension in extensions {
        let object = require_object(Some(extension), "run_definition extension")?;
        let namespace = require_string(object.get("namespace"), "extension namespace")?;
        validate_extension_namespace(namespace)?;
        require_string(object.get("version"), "extension version")?;
        if !object.get("required").is_some_and(Value::is_boolean) {
            return Err(CheckpointError::new(
                "checkpoint_definition_invalid",
                "extension required must be boolean",
            ));
        }
        if let Some(previous) = previous {
            if previous >= namespace {
                return Err(CheckpointError::new(
                    "checkpoint_definition_invalid",
                    "extensions must be sorted and unique by namespace",
                ));
            }
        }
        previous = Some(namespace);
    }
    Ok(())
}

fn validate_tool_policy(value: Option<&Value>) -> CheckpointResult<()> {
    let object = require_object(value, "run_definition.tool_policy")?;
    if let Some(allowed) = object.get("allowed_tools") {
        if !allowed.is_null() {
            validate_sorted_unique_strings(Some(allowed), "tool_policy.allowed_tools")?;
        }
    }
    validate_sorted_unique_strings(
        object.get("disallowed_tools"),
        "tool_policy.disallowed_tools",
    )?;
    if let Some(predicate) = object.get("predicate_ref") {
        if !predicate.is_null() {
            validate_ref_value(predicate, "tool_policy.predicate_ref")?;
        }
    }
    Ok(())
}

fn validate_checkpoint_policy(value: Option<&Value>) -> CheckpointResult<()> {
    let object = require_object(value, "run_definition.checkpoint_policy")?;
    for key in ["ambiguous_model_policy", "ambiguous_tool_policy"] {
        require_string(object.get(key), &format!("checkpoint_policy.{key}"))?;
    }
    let max = object.get("max_extension_state_bytes").ok_or_else(|| {
        CheckpointError::new(
            "checkpoint_definition_invalid",
            "checkpoint_policy.max_extension_state_bytes is required",
        )
    })?;
    let Some(max) = max.as_u64() else {
        return Err(CheckpointError::new(
            "checkpoint_definition_not_i_json",
            "max_extension_state_bytes must be a safe integer",
        ));
    };
    if max > MAX_WIRE_INTEGER {
        return Err(CheckpointError::new(
            "checkpoint_definition_not_i_json",
            "max_extension_state_bytes exceeds the safe integer range",
        ));
    }
    Ok(())
}

fn validate_header_names(model: Option<&Value>) -> CheckpointResult<()> {
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

fn validate_credential_slots(definition: &Value, slots: &[Value]) -> CheckpointResult<()> {
    let mut previous: Option<&str> = None;
    for slot in slots {
        let slot = slot.as_str().ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_credential_slots_invalid",
                "credential slots must be strings",
            )
        })?;
        if let Some(previous) = previous {
            if utf16_cmp(previous, slot) != Ordering::Less {
                return Err(CheckpointError::new(
                    "checkpoint_credential_slots_invalid",
                    "credential slots must be sorted and unique",
                ));
            }
        }
        validate_pointer(slot)?;
        let resolved = resolve_json_pointer(definition, slot)?;
        if resolved.as_str() != Some(CREDENTIAL_REDACTED) {
            return Err(CheckpointError::new(
                "checkpoint_credential_value_not_redacted",
                format!("credential slot {slot} is not redacted"),
            ));
        }
        previous = Some(slot);
    }
    Ok(())
}

fn validate_sorted_unique_strings(value: Option<&Value>, field_name: &str) -> CheckpointResult<()> {
    let values = require_array(value, field_name)?;
    let mut previous: Option<&str> = None;
    for value in values {
        let string = require_string(Some(value), field_name)?;
        if let Some(previous) = previous {
            if utf16_cmp(previous, string) != Ordering::Less {
                return Err(CheckpointError::new(
                    "checkpoint_definition_invalid",
                    format!("{field_name} must be sorted and unique"),
                ));
            }
        }
        previous = Some(string);
    }
    Ok(())
}

fn validate_ref_value(value: &Value, field_name: &str) -> CheckpointResult<()> {
    let object = require_object(Some(value), field_name)?;
    if object.len() != 2 || !object.contains_key("id") || !object.contains_key("version") {
        return Err(CheckpointError::new(
            "checkpoint_capability_ref_invalid",
            format!("{field_name} must contain exactly id and version"),
        ));
    }
    require_string(object.get("id"), &format!("{field_name}.id"))?;
    require_string(object.get("version"), &format!("{field_name}.version"))?;
    Ok(())
}

pub(super) fn validate_pointer(pointer: &str) -> CheckpointResult<()> {
    pointer_tokens(pointer).map(|_| ())
}

fn pointer_tokens(pointer: &str) -> CheckpointResult<Vec<String>> {
    if pointer.is_empty() {
        return Ok(Vec::new());
    }
    if !pointer.starts_with('/') {
        return Err(CheckpointError::new(
            "checkpoint_credential_slots_invalid",
            "JSON pointer must be empty or start with '/'",
        ));
    }
    pointer
        .split('/')
        .skip(1)
        .map(|raw| {
            let mut token = String::with_capacity(raw.len());
            let bytes = raw.as_bytes();
            let mut index = 0;
            while index < bytes.len() {
                if bytes[index] != b'~' {
                    let character = raw[index..].chars().next().expect("valid UTF-8");
                    token.push(character);
                    index += character.len_utf8();
                    continue;
                }
                if index + 1 >= bytes.len() {
                    return Err(CheckpointError::new(
                        "checkpoint_credential_slots_invalid",
                        "JSON pointer contains an invalid escape",
                    ));
                }
                match bytes[index + 1] {
                    b'0' => token.push('~'),
                    b'1' => token.push('/'),
                    _ => {
                        return Err(CheckpointError::new(
                            "checkpoint_credential_slots_invalid",
                            "JSON pointer contains an invalid escape",
                        ));
                    }
                }
                index += 2;
            }
            Ok(token)
        })
        .collect()
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

fn normalize_tool_policy(policy: Option<&mut Value>) -> CheckpointResult<()> {
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

fn require_string<'a>(value: Option<&'a Value>, field_name: &str) -> CheckpointResult<&'a str> {
    value.and_then(Value::as_str).ok_or_else(|| {
        CheckpointError::new(
            "checkpoint_definition_invalid",
            format!("{field_name} must be a string"),
        )
    })
}

fn require_object<'a>(
    value: Option<&'a Value>,
    field_name: &str,
) -> CheckpointResult<&'a Map<String, Value>> {
    value.and_then(Value::as_object).ok_or_else(|| {
        CheckpointError::new(
            "checkpoint_definition_invalid",
            format!("{field_name} must be an object"),
        )
    })
}

fn require_array<'a>(
    value: Option<&'a Value>,
    field_name: &str,
) -> CheckpointResult<&'a Vec<Value>> {
    value.and_then(Value::as_array).ok_or_else(|| {
        CheckpointError::new(
            "checkpoint_definition_invalid",
            format!("{field_name} must be an array"),
        )
    })
}

fn validate_optional_object_or_null(
    value: Option<&Value>,
    field_name: &str,
) -> CheckpointResult<()> {
    if value.is_some_and(|value| !value.is_object() && !value.is_null()) {
        return Err(CheckpointError::new(
            "checkpoint_definition_invalid",
            format!("{field_name} must be an object or null"),
        ));
    }
    Ok(())
}

pub(super) fn utf16_cmp(left: &str, right: &str) -> Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}
