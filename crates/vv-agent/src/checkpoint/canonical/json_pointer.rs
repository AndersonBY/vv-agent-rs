use super::*;

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

pub(in crate::checkpoint) fn validate_pointer(pointer: &str) -> CheckpointResult<()> {
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
