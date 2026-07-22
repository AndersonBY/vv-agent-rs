use super::*;

pub(super) fn behavior_metadata(agent: &Agent, run_config: &RunConfig) -> Value {
    let mut metadata = agent.metadata().clone();
    metadata.extend(run_config.metadata.clone());
    for key in RUNTIME_METADATA_KEYS {
        metadata.remove(*key);
    }
    Value::Object(metadata.into_iter().collect())
}

pub(super) fn output_schema(agent: &Agent, settings: &ModelSettings) -> Option<Value> {
    if let Some(ResponseFormat::JsonSchema { json_schema }) = &settings.response_format {
        return Some(Value::Object(json_schema.clone()));
    }
    let output_type = agent.output_type_name()?;
    if output_type.ends_with("String") || output_type == "str" {
        return Some(json!({"type": "string"}));
    }
    Some(json!({"type": "string", "x-rust-output-type": output_type}))
}

pub(super) fn require_declared_credential_headers(
    definition: &Value,
    credential_slots: &[String],
) -> CheckpointResult<()> {
    let Some(headers) = definition
        .pointer("/model/settings/extra_headers")
        .and_then(Value::as_object)
    else {
        return Ok(());
    };
    let declared = credential_slots
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let missing = headers
        .keys()
        .filter(|name| KNOWN_CREDENTIAL_HEADERS.contains(&name.as_str()))
        .filter(|name| {
            let pointer = format!("/model/settings/extra_headers/{name}");
            !declared.contains(pointer.as_str())
        })
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(CheckpointError::new(
            "checkpoint_definition_unstable",
            format!(
                "model credential headers require explicit credential_slots: {}",
                missing.join(", ")
            ),
        ));
    }
    Ok(())
}
