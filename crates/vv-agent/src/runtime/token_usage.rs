use serde_json::Value;

use crate::types::{CycleRecord, TaskTokenUsage, TokenUsage};

pub fn normalize_token_usage(raw_usage: &Value) -> TokenUsage {
    let Some(raw) = raw_usage.as_object() else {
        return TokenUsage::default();
    };

    let prompt_tokens = read_int(raw.get("prompt_tokens"));
    let completion_tokens = read_int(raw.get("completion_tokens"));
    let input_tokens = read_int(raw.get("input_tokens")).or(prompt_tokens);
    let output_tokens = read_int(raw.get("output_tokens")).or(completion_tokens);
    let total_tokens = read_int(raw.get("total_tokens")).unwrap_or_else(|| {
        prompt_tokens.or(input_tokens).unwrap_or_default()
            + completion_tokens.or(output_tokens).unwrap_or_default()
    });
    let cached_tokens = read_nested_int(
        raw_usage,
        &[
            &["prompt_tokens_details", "cached_tokens"],
            &["input_tokens_details", "cached_tokens"],
            &["cache_read_input_tokens"],
            &["cache_read_tokens"],
        ],
    )
    .unwrap_or_default();
    let reasoning_tokens = read_nested_int(
        raw_usage,
        &[
            &["completion_tokens_details", "reasoning_tokens"],
            &["output_tokens_details", "reasoning_tokens"],
            &["reasoning_tokens"],
        ],
    )
    .unwrap_or_default();
    let cache_creation_tokens = read_nested_int(
        raw_usage,
        &[
            &["input_tokens_details", "cache_creation_tokens"],
            &["prompt_tokens_details", "cache_creation_tokens"],
            &["cache_creation_input_tokens"],
            &["cache_creation_tokens"],
        ],
    )
    .unwrap_or_default();

    TokenUsage {
        prompt_tokens: prompt_tokens.or(input_tokens).unwrap_or_default(),
        completion_tokens: completion_tokens.or(output_tokens).unwrap_or_default(),
        total_tokens,
        cached_tokens,
        reasoning_tokens,
        input_tokens: input_tokens.unwrap_or_default(),
        output_tokens: output_tokens.unwrap_or_default(),
        cache_creation_tokens,
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
    for path in path_options {
        let mut current = source;
        let mut matched = true;
        for key in *path {
            let Some(next) = current.as_object().and_then(|object| object.get(*key)) else {
                matched = false;
                break;
            };
            current = next;
        }
        if matched {
            if let Some(value) = read_int(Some(current)) {
                return Some(value);
            }
        }
    }
    None
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
        .and_then(|value| {
            if value >= 0.0 {
                Some(value as u64)
            } else {
                None
            }
        })
}
