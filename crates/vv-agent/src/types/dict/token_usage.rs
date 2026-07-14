use super::common::*;
use super::*;

pub(super) fn token_usage_to_dict(usage: &TokenUsage) -> Value {
    Value::Object(serde_json::Map::from_iter([
        (
            "prompt_tokens".to_string(),
            Value::from(usage.prompt_tokens),
        ),
        (
            "completion_tokens".to_string(),
            Value::from(usage.completion_tokens),
        ),
        ("total_tokens".to_string(), Value::from(usage.total_tokens)),
        (
            "cached_tokens".to_string(),
            Value::from(usage.cached_tokens),
        ),
        (
            "reasoning_tokens".to_string(),
            Value::from(usage.reasoning_tokens),
        ),
        ("input_tokens".to_string(), Value::from(usage.input_tokens)),
        (
            "output_tokens".to_string(),
            Value::from(usage.output_tokens),
        ),
        (
            "cache_creation_tokens".to_string(),
            Value::from(usage.cache_creation_tokens),
        ),
        (
            "usage_source".to_string(),
            serde_json::to_value(usage.usage_source).expect("usage source is serializable"),
        ),
        (
            "cache_usage".to_string(),
            serde_json::to_value(&usage.cache_usage).expect("cache usage is serializable"),
        ),
        ("raw".to_string(), usage.raw.clone()),
    ]))
}

pub(super) fn token_usage_from_dict(value: &Value) -> Result<TokenUsage, String> {
    let object = expect_object(value, "TokenUsage")?;
    Ok(TokenUsage {
        prompt_tokens: read_u64(object, "prompt_tokens", 0),
        completion_tokens: read_u64(object, "completion_tokens", 0),
        total_tokens: read_u64(object, "total_tokens", 0),
        cached_tokens: read_u64(object, "cached_tokens", 0),
        reasoning_tokens: read_u64(object, "reasoning_tokens", 0),
        input_tokens: read_u64(object, "input_tokens", 0),
        output_tokens: read_u64(object, "output_tokens", 0),
        cache_creation_tokens: read_u64(object, "cache_creation_tokens", 0),
        usage_source: read_json_field(object, "usage_source")?,
        cache_usage: read_json_field(object, "cache_usage")?,
        raw: object
            .get("raw")
            .cloned()
            .unwrap_or_else(|| Value::Object(Default::default())),
    })
}

pub(super) fn task_token_usage_to_dict(usage: &TaskTokenUsage) -> Value {
    Value::Object(serde_json::Map::from_iter([
        (
            "prompt_tokens".to_string(),
            Value::from(usage.prompt_tokens),
        ),
        (
            "completion_tokens".to_string(),
            Value::from(usage.completion_tokens),
        ),
        ("total_tokens".to_string(), Value::from(usage.total_tokens)),
        (
            "cached_tokens".to_string(),
            Value::from(usage.cached_tokens),
        ),
        (
            "reasoning_tokens".to_string(),
            Value::from(usage.reasoning_tokens),
        ),
        ("input_tokens".to_string(), Value::from(usage.input_tokens)),
        (
            "output_tokens".to_string(),
            Value::from(usage.output_tokens),
        ),
        (
            "cache_creation_tokens".to_string(),
            Value::from(usage.cache_creation_tokens),
        ),
        (
            "cache_usage".to_string(),
            serde_json::to_value(&usage.cache_usage).expect("cache usage is serializable"),
        ),
        (
            "cycles".to_string(),
            Value::Array(
                usage
                    .cycles
                    .iter()
                    .map(|cycle| {
                        let mut payload = match token_usage_to_dict(&cycle.usage) {
                            Value::Object(map) => map,
                            _ => serde_json::Map::new(),
                        };
                        payload.insert("cycle_index".to_string(), Value::from(cycle.cycle_index));
                        Value::Object(payload)
                    })
                    .collect(),
            ),
        ),
    ]))
}

pub(super) fn task_token_usage_from_dict(value: &Value) -> Result<TaskTokenUsage, String> {
    let object = expect_object(value, "TaskTokenUsage")?;
    let cycles = read_array(object, "cycles")
        .unwrap_or(&[])
        .iter()
        .map(|cycle| {
            let cycle_object = expect_object(cycle, "CycleTokenUsage")?;
            Ok(CycleTokenUsage {
                cycle_index: read_u32(cycle_object, "cycle_index", 0),
                usage: token_usage_from_dict(cycle)?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(TaskTokenUsage {
        prompt_tokens: read_u64(object, "prompt_tokens", 0),
        completion_tokens: read_u64(object, "completion_tokens", 0),
        total_tokens: read_u64(object, "total_tokens", 0),
        cached_tokens: read_u64(object, "cached_tokens", 0),
        reasoning_tokens: read_u64(object, "reasoning_tokens", 0),
        input_tokens: read_u64(object, "input_tokens", 0),
        output_tokens: read_u64(object, "output_tokens", 0),
        cache_creation_tokens: read_u64(object, "cache_creation_tokens", 0),
        cache_usage: read_json_field(object, "cache_usage")?,
        cycles,
    })
}

fn read_json_field<T>(object: &serde_json::Map<String, Value>, key: &str) -> Result<T, String>
where
    T: serde::de::DeserializeOwned + Default,
{
    object.get(key).map_or_else(
        || Ok(T::default()),
        |value| {
            serde_json::from_value(value.clone()).map_err(|error| format!("invalid {key}: {error}"))
        },
    )
}
