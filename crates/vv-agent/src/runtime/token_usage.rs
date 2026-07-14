use serde_json::Value;

use crate::types::{
    CacheUsage, CacheUsageStatus, CycleRecord, TaskTokenUsage, TokenUsage, UsageSource,
};

pub fn normalize_token_usage(raw_usage: &Value) -> TokenUsage {
    normalize_token_usage_with_hints(raw_usage, None, None)
}

pub fn normalize_token_usage_with_hints(
    raw_usage: &Value,
    usage_source: Option<UsageSource>,
    cache_status: Option<CacheUsageStatus>,
) -> TokenUsage {
    let Some(raw) = raw_usage.as_object() else {
        let status = cache_status.unwrap_or_default();
        return TokenUsage {
            usage_source: usage_source.unwrap_or_default(),
            cache_usage: CacheUsage {
                status,
                source: (status == CacheUsageStatus::Unsupported)
                    .then(|| "adapter_capability".to_string()),
                ..CacheUsage::default()
            },
            ..TokenUsage::default()
        };
    };

    let prompt_tokens = read_int(raw.get("prompt_tokens"));
    let completion_tokens = read_int(raw.get("completion_tokens"));
    let input_tokens = read_int(raw.get("input_tokens")).or(prompt_tokens);
    let output_tokens = read_int(raw.get("output_tokens")).or(completion_tokens);
    let total_tokens = read_int(raw.get("total_tokens")).unwrap_or_else(|| {
        prompt_tokens.or(input_tokens).unwrap_or_default()
            + completion_tokens.or(output_tokens).unwrap_or_default()
    });
    let cached_tokens = read_nested_cache_int(
        raw_usage,
        &[
            &["cache_read_tokens"],
            &["prompt_tokens_details", "cached_tokens"],
            &["input_tokens_details", "cached_tokens"],
            &["cache_read_input_tokens"],
        ],
    );
    let reasoning_tokens = read_nested_int(
        raw_usage,
        &[
            &["completion_tokens_details", "reasoning_tokens"],
            &["output_tokens_details", "reasoning_tokens"],
            &["reasoning_tokens"],
        ],
    )
    .unwrap_or_default();
    let cache_creation_tokens = read_nested_cache_int(
        raw_usage,
        &[
            &["cache_creation_tokens"],
            &["cache_write_tokens"],
            &["input_tokens_details", "cache_creation_tokens"],
            &["prompt_tokens_details", "cache_creation_tokens"],
            &["cache_creation_input_tokens"],
            &["cache_write_input_tokens"],
        ],
    );
    let mut uncached_input_tokens = read_nested_cache_int(raw_usage, &[&["uncached_input_tokens"]]);
    let observed_cache_metric = cached_tokens.is_some()
        || cache_creation_tokens.is_some()
        || uncached_input_tokens.is_some();
    let normalized_cache_status = if observed_cache_metric {
        CacheUsageStatus::ProviderReported
    } else {
        cache_status.unwrap_or_default()
    };

    if normalized_cache_status == CacheUsageStatus::ProviderReported
        && uncached_input_tokens.is_none()
    {
        if has_any_key(
            raw_usage,
            &[
                "cache_read_input_tokens",
                "cache_creation_input_tokens",
                "cache_write_input_tokens",
            ],
        ) {
            uncached_input_tokens = input_tokens;
        } else if (has_nested_path(raw_usage, &["prompt_tokens_details", "cached_tokens"])
            || has_nested_path(raw_usage, &["input_tokens_details", "cached_tokens"]))
            && input_tokens.is_some()
            && cached_tokens.is_some()
        {
            uncached_input_tokens = Some(
                input_tokens
                    .unwrap_or_default()
                    .saturating_sub(cached_tokens.unwrap_or_default()),
            );
        }
    }

    let cache_usage = CacheUsage {
        status: normalized_cache_status,
        read_tokens: (normalized_cache_status == CacheUsageStatus::ProviderReported)
            .then_some(cached_tokens)
            .flatten(),
        write_tokens: (normalized_cache_status == CacheUsageStatus::ProviderReported)
            .then_some(cache_creation_tokens)
            .flatten(),
        uncached_input_tokens: (normalized_cache_status == CacheUsageStatus::ProviderReported)
            .then_some(uncached_input_tokens)
            .flatten(),
        source: match normalized_cache_status {
            CacheUsageStatus::ProviderReported => Some("provider_usage".to_string()),
            CacheUsageStatus::Unsupported => Some("adapter_capability".to_string()),
            CacheUsageStatus::AccountingMissing => None,
        },
    };

    TokenUsage {
        prompt_tokens: prompt_tokens.or(input_tokens).unwrap_or_default(),
        completion_tokens: completion_tokens.or(output_tokens).unwrap_or_default(),
        total_tokens,
        cached_tokens: cached_tokens.unwrap_or_default(),
        reasoning_tokens,
        input_tokens: input_tokens.unwrap_or_default(),
        output_tokens: output_tokens.unwrap_or_default(),
        cache_creation_tokens: cache_creation_tokens.unwrap_or_default(),
        usage_source: usage_source.unwrap_or_else(|| infer_usage_source(raw_usage)),
        cache_usage,
        raw: raw_usage.clone(),
    }
}

pub fn summarize_task_token_usage(cycles: &[CycleRecord]) -> TaskTokenUsage {
    let mut summary = TaskTokenUsage::default();
    for cycle in cycles {
        summary.add_cycle(cycle.index, cycle.token_usage.clone());
    }
    summary
}

fn read_nested_int(source: &Value, path_options: &[&[&str]]) -> Option<u64> {
    path_options
        .iter()
        .find_map(|path| nested_value(source, path).and_then(|value| read_int(Some(value))))
}

fn read_nested_cache_int(source: &Value, path_options: &[&[&str]]) -> Option<u64> {
    path_options
        .iter()
        .find_map(|path| nested_value(source, path).and_then(read_cache_int))
}

fn nested_value<'a>(source: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = source;
    for key in path {
        current = current.as_object()?.get(*key)?;
    }
    Some(current)
}

fn read_cache_int(value: &Value) -> Option<u64> {
    match value {
        Value::Bool(_) => None,
        Value::Number(number) => number.as_u64().or_else(|| {
            number.as_f64().and_then(|value| {
                (value.is_finite() && value >= 0.0 && value.fract() == 0.0).then_some(value as u64)
            })
        }),
        Value::String(value) => value.trim().parse::<u64>().ok(),
        _ => None,
    }
}

fn read_int(value: Option<&Value>) -> Option<u64> {
    match value? {
        Value::Bool(_) => None,
        Value::Number(number) => number
            .as_u64()
            .or_else(|| {
                number
                    .as_i64()
                    .and_then(|value| (value >= 0).then_some(value as u64))
            })
            .or_else(|| number.as_f64().and_then(float_to_u64)),
        Value::String(value) => value.trim().parse::<u64>().ok(),
        _ => None,
    }
}

fn float_to_u64(value: f64) -> Option<u64> {
    value
        .is_finite()
        .then_some(value.trunc())
        .and_then(|value| (value >= 0.0).then_some(value as u64))
}

fn infer_usage_source(raw_usage: &Value) -> UsageSource {
    let Some(raw) = raw_usage.as_object() else {
        return UsageSource::AccountingMissing;
    };
    if [
        "prompt_tokens",
        "completion_tokens",
        "total_tokens",
        "input_tokens",
        "output_tokens",
    ]
    .iter()
    .any(|key| raw.contains_key(*key) && read_int(raw.get(*key)).is_some())
    {
        UsageSource::ProviderReported
    } else {
        UsageSource::default()
    }
}

fn has_any_key(source: &Value, keys: &[&str]) -> bool {
    source
        .as_object()
        .is_some_and(|object| keys.iter().any(|key| object.contains_key(*key)))
}

fn has_nested_path(source: &Value, path: &[&str]) -> bool {
    nested_value(source, path).is_some()
}
