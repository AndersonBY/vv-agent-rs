use serde_json::Value;

use crate::types::{
    CacheUsage, CacheUsageStatus, ModelCallRecord, TaskTokenUsage, TokenUsage, UsageSource,
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

    let prompt_tokens = read_count(raw.get("prompt_tokens"));
    let completion_tokens = read_count(raw.get("completion_tokens"));
    let native_input_tokens = read_count(raw.get("input_tokens")).or(prompt_tokens);
    let output_tokens = read_count(raw.get("output_tokens")).or(completion_tokens);
    let cache_read_input_tokens = read_nested_count(
        raw_usage,
        &[
            &["cache_read_input_tokens"],
            &["cache_read_tokens"],
            &["prompt_tokens_details", "cached_tokens"],
            &["input_tokens_details", "cached_tokens"],
        ],
    );
    let reasoning_tokens = read_nested_count(
        raw_usage,
        &[
            &["completion_tokens_details", "reasoning_tokens"],
            &["output_tokens_details", "reasoning_tokens"],
            &["reasoning_tokens"],
        ],
    );
    let cache_write_input_tokens = read_nested_count(
        raw_usage,
        &[
            &["cache_write_input_tokens"],
            &["cache_creation_input_tokens"],
            &["cache_write_tokens"],
            &["input_tokens_details", "cache_creation_tokens"],
            &["prompt_tokens_details", "cache_creation_tokens"],
        ],
    );
    let mut uncached_input_tokens = read_nested_count(raw_usage, &[&["uncached_input_tokens"]]);

    let anthropic_native = prompt_tokens.is_none()
        && !raw.contains_key("total_tokens")
        && uncached_input_tokens.is_none()
        && has_any_key(
            raw_usage,
            &["cache_read_input_tokens", "cache_creation_input_tokens"],
        );
    let input_tokens = if anthropic_native {
        native_input_tokens.and_then(|native| {
            native
                .checked_add(cache_read_input_tokens.unwrap_or_default())?
                .checked_add(cache_write_input_tokens.unwrap_or_default())
        })
    } else {
        native_input_tokens
    };
    let total_tokens = read_count(raw.get("total_tokens")).or_else(|| {
        input_tokens.and_then(|input| output_tokens.and_then(|output| input.checked_add(output)))
    });

    let observed_cache_metric = cache_read_input_tokens.is_some()
        || cache_write_input_tokens.is_some()
        || uncached_input_tokens.is_some();
    let normalized_cache_status = if observed_cache_metric {
        CacheUsageStatus::ProviderReported
    } else {
        cache_status.unwrap_or_default()
    };

    if normalized_cache_status == CacheUsageStatus::ProviderReported
        && uncached_input_tokens.is_none()
    {
        if anthropic_native {
            uncached_input_tokens = native_input_tokens.and_then(|native| {
                native.checked_add(cache_write_input_tokens.unwrap_or_default())
            });
        } else if let (Some(input), Some(read)) = (input_tokens, cache_read_input_tokens) {
            uncached_input_tokens = Some(input.saturating_sub(read));
        }
    }

    let cache_usage = CacheUsage {
        status: normalized_cache_status,
        read_input_tokens: (normalized_cache_status == CacheUsageStatus::ProviderReported)
            .then_some(cache_read_input_tokens)
            .flatten(),
        write_input_tokens: (normalized_cache_status == CacheUsageStatus::ProviderReported)
            .then_some(cache_write_input_tokens)
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
        input_tokens,
        output_tokens,
        total_tokens,
        reasoning_tokens,
        usage_source: usage_source.unwrap_or_else(|| infer_usage_source(raw_usage)),
        cache_usage,
        provider_usage: raw.clone(),
    }
}

pub fn summarize_task_token_usage(model_calls: &[ModelCallRecord]) -> TaskTokenUsage {
    let mut summary = TaskTokenUsage::default();
    for model_call in model_calls {
        summary
            .add_model_call(model_call.clone())
            .expect("runtime model call ledger contains duplicate call_id");
    }
    summary
}

fn read_nested_count(source: &Value, path_options: &[&[&str]]) -> Option<u64> {
    path_options
        .iter()
        .find_map(|path| nested_value(source, path).and_then(|value| read_count(Some(value))))
}

fn nested_value<'a>(source: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = source;
    for key in path {
        current = current.as_object()?.get(*key)?;
    }
    Some(current)
}

fn read_count(value: Option<&Value>) -> Option<u64> {
    match value? {
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
    .any(|key| raw.contains_key(*key) && read_count(raw.get(*key)).is_some())
    {
        UsageSource::ProviderReported
    } else {
        UsageSource::AccountingMissing
    }
}

fn has_any_key(source: &Value, keys: &[&str]) -> bool {
    source
        .as_object()
        .is_some_and(|object| keys.iter().any(|key| object.contains_key(*key)))
}
