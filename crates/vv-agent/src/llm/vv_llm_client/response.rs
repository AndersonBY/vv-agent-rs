use serde_json::Value;

use crate::memory::token_utils::count_tokens;
use crate::runtime::normalize_token_usage_with_hints;
use crate::types::{LLMResponse, Metadata, TokenUsage, ToolCall, UsageSource};

#[derive(Debug, Clone)]
pub(super) struct UsageEstimateContext {
    pub(super) model: String,
    pub(super) prompt_tokens: u64,
}

pub(super) fn from_vv_llm_response(
    response: vv_llm::ChatResponse,
    estimate: Option<UsageEstimateContext>,
) -> LLMResponse {
    let mut raw = Metadata::new();
    raw.insert("id".to_string(), Value::String(response.id));
    raw.insert("model".to_string(), Value::String(response.model));
    if let Some(reasoning_content) = response
        .reasoning_content
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        raw.insert(
            "reasoning_content".to_string(),
            Value::String(reasoning_content.to_string()),
        );
    }
    let token_usage = response.usage.map(from_vv_llm_usage).unwrap_or_else(|| {
        estimate_missing_usage(&response.content, &response.tool_calls, estimate)
    });
    raw.insert("usage".to_string(), token_usage.raw.clone());
    LLMResponse {
        content: response.content,
        tool_calls: response
            .tool_calls
            .into_iter()
            .enumerate()
            .map(|(index, tool_call)| from_vv_llm_tool_call(tool_call, index))
            .collect(),
        raw,
        token_usage,
    }
}

pub(super) fn from_vv_llm_tool_call(tool_call: vv_llm::ToolCall, index: usize) -> ToolCall {
    let mut normalized = ToolCall::from_raw_arguments(
        normalize_tool_call_id(&tool_call.id, index),
        normalize_tool_call_name(&tool_call.name),
        Value::String(tool_call.arguments),
    );
    if let Some(extra_content) = tool_call.extra_content {
        normalized.extra_content = Some(match normalized.extra_content.take() {
            Some(existing) => merge_tool_call_extra_content(existing, extra_content),
            None => extra_content,
        });
    }
    normalized
}

pub(super) fn merge_tool_call_extra_content(existing: Value, extra_content: Value) -> Value {
    match (existing, extra_content) {
        (Value::Object(mut existing), Value::Object(extra)) => {
            existing.extend(extra);
            Value::Object(existing)
        }
        (existing, extra_content) => {
            serde_json::json!({"parse_error": existing, "provider_extra_content": extra_content})
        }
    }
}

fn normalize_tool_call_id(id: &str, index: usize) -> String {
    let id = id.trim();
    if id.is_empty() {
        format!("call_generated_{index}")
    } else {
        id.to_string()
    }
}

fn normalize_tool_call_name(name: &str) -> String {
    name.replace(' ', "")
}

pub(super) fn from_vv_llm_usage(usage: vv_llm::ChatUsage) -> TokenUsage {
    let raw = usage
        .raw_usage
        .clone()
        .unwrap_or_else(|| serde_json::to_value(&usage).unwrap_or_else(|_| serde_json::json!({})));
    let mut normalization_input = raw.clone();
    overlay_typed_cache_usage(&mut normalization_input, &usage);
    let mut normalized = normalize_token_usage_with_hints(
        &normalization_input,
        Some(UsageSource::ProviderReported),
        None,
    );
    normalized.prompt_tokens = usage
        .prompt_tokens
        .map(u64::from)
        .unwrap_or(normalized.prompt_tokens);
    normalized.completion_tokens = usage
        .completion_tokens
        .map(u64::from)
        .unwrap_or(normalized.completion_tokens);
    normalized.total_tokens = usage
        .total_tokens
        .map(u64::from)
        .unwrap_or(normalized.total_tokens);
    normalized.input_tokens = usage
        .input_tokens
        .or(usage.prompt_tokens)
        .map(u64::from)
        .unwrap_or(normalized.input_tokens);
    normalized.output_tokens = usage
        .output_tokens
        .or(usage.completion_tokens)
        .map(u64::from)
        .unwrap_or(normalized.output_tokens);
    normalized.raw = raw;
    normalized
}

fn overlay_typed_cache_usage(raw: &mut Value, usage: &vv_llm::ChatUsage) {
    let Some(raw) = raw.as_object_mut() else {
        return;
    };
    // Anthropic also fills legacy prompt fields; only native raw keys identify input semantics.
    let openai_usage = raw.contains_key("prompt_tokens")
        || raw.contains_key("completion_tokens")
        || raw.contains_key("prompt_tokens_details");

    if let Some(cache_read) = usage.cache_read_input_tokens {
        if openai_usage {
            insert_token_detail(raw, "cached_tokens", cache_read);
        } else {
            raw.insert(
                "cache_read_input_tokens".to_string(),
                Value::from(cache_read),
            );
        }
    }
    if let Some(cache_creation) = usage.cache_creation_input_tokens {
        if openai_usage {
            insert_token_detail(raw, "cache_creation_tokens", cache_creation);
        } else {
            raw.insert(
                "cache_creation_input_tokens".to_string(),
                Value::from(cache_creation),
            );
        }
    }
}

fn insert_token_detail(raw: &mut serde_json::Map<String, Value>, key: &str, value: u32) {
    let details = raw
        .entry("prompt_tokens_details".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !details.is_object() {
        *details = Value::Object(serde_json::Map::new());
    }
    details
        .as_object_mut()
        .expect("token details were normalized to an object")
        .insert(key.to_string(), Value::from(value));
}

pub(super) fn estimate_missing_usage(
    content: &str,
    tool_calls: &[vv_llm::ToolCall],
    estimate: Option<UsageEstimateContext>,
) -> TokenUsage {
    let Some(estimate) = estimate else {
        return TokenUsage::default();
    };
    let completion_payload = completion_payload_for_usage(content, tool_calls);
    let completion_tokens = count_tokens(&completion_payload, &estimate.model);
    let total_tokens = estimate.prompt_tokens + completion_tokens;
    let raw = serde_json::json!({
        "prompt_tokens": estimate.prompt_tokens,
        "completion_tokens": completion_tokens,
        "total_tokens": total_tokens,
    });
    TokenUsage {
        prompt_tokens: estimate.prompt_tokens,
        completion_tokens,
        total_tokens,
        input_tokens: estimate.prompt_tokens,
        output_tokens: completion_tokens,
        usage_source: UsageSource::Estimated,
        raw,
        ..TokenUsage::default()
    }
}

pub(super) fn completion_payload_for_usage(
    content: &str,
    tool_calls: &[vv_llm::ToolCall],
) -> String {
    if tool_calls.is_empty() {
        return content.to_string();
    }
    let mut payload = content.to_string();
    if let Ok(tool_payload) = serde_json::to_string(tool_calls) {
        if !payload.is_empty() {
            payload.push('\n');
        }
        payload.push_str(&tool_payload);
    }
    payload
}

#[cfg(test)]
mod tests {
    use super::from_vv_llm_usage;
    use crate::types::{CacheUsageStatus, UsageSource};

    #[test]
    fn typed_openai_cache_zero_is_accounted_without_rewriting_raw_usage() {
        let raw = serde_json::json!({
            "prompt_tokens": 11,
            "completion_tokens": 7,
            "total_tokens": 18
        });
        let normalized = from_vv_llm_usage(vv_llm::ChatUsage {
            prompt_tokens: Some(11),
            completion_tokens: Some(7),
            total_tokens: Some(18),
            input_tokens: Some(11),
            output_tokens: Some(7),
            cache_read_input_tokens: Some(0),
            cache_creation_input_tokens: None,
            raw_usage: Some(raw.clone()),
        });

        assert_eq!(normalized.usage_source, UsageSource::ProviderReported);
        assert_eq!(
            normalized.cache_usage.status,
            CacheUsageStatus::ProviderReported
        );
        assert_eq!(normalized.cache_usage.read_tokens, Some(0));
        assert_eq!(normalized.cache_usage.uncached_input_tokens, Some(11));
        assert_eq!(normalized.raw, raw);
    }

    #[test]
    fn typed_anthropic_cache_keeps_input_tokens_as_uncached_input() {
        let raw = serde_json::json!({
            "input_tokens": 11,
            "output_tokens": 7
        });
        let normalized = from_vv_llm_usage(vv_llm::ChatUsage {
            prompt_tokens: Some(11),
            completion_tokens: Some(7),
            total_tokens: Some(18),
            input_tokens: Some(11),
            output_tokens: Some(7),
            cache_read_input_tokens: Some(5),
            cache_creation_input_tokens: Some(3),
            raw_usage: Some(raw.clone()),
        });

        assert_eq!(normalized.cache_usage.read_tokens, Some(5));
        assert_eq!(normalized.cache_usage.write_tokens, Some(3));
        assert_eq!(normalized.cache_usage.uncached_input_tokens, Some(11));
        assert_eq!(normalized.raw, raw);
    }

    #[test]
    fn typed_anthropic_cache_precedes_invalid_raw_without_mutating_it() {
        let raw = serde_json::json!({
            "input_tokens": 11,
            "output_tokens": 7,
            "cache_read_input_tokens": "invalid",
            "cache_creation_input_tokens": null
        });
        let normalized = from_vv_llm_usage(vv_llm::ChatUsage {
            prompt_tokens: Some(11),
            completion_tokens: Some(7),
            total_tokens: Some(18),
            input_tokens: Some(11),
            output_tokens: Some(7),
            cache_read_input_tokens: Some(5),
            cache_creation_input_tokens: Some(3),
            raw_usage: Some(raw.clone()),
        });

        assert_eq!(normalized.cache_usage.read_tokens, Some(5));
        assert_eq!(normalized.cache_usage.write_tokens, Some(3));
        assert_eq!(normalized.cache_usage.uncached_input_tokens, Some(11));
        assert_eq!(normalized.raw, raw);
    }

    #[test]
    fn typed_cache_value_precedes_invalid_raw_detail_without_mutating_it() {
        let raw = serde_json::json!({
            "prompt_tokens": 11,
            "completion_tokens": 7,
            "total_tokens": 18,
            "prompt_tokens_details": {"cached_tokens": "invalid"}
        });
        let normalized = from_vv_llm_usage(vv_llm::ChatUsage {
            prompt_tokens: Some(11),
            completion_tokens: Some(7),
            total_tokens: Some(18),
            input_tokens: Some(11),
            output_tokens: Some(7),
            cache_read_input_tokens: Some(5),
            cache_creation_input_tokens: None,
            raw_usage: Some(raw.clone()),
        });

        assert_eq!(normalized.cache_usage.read_tokens, Some(5));
        assert_eq!(normalized.cache_usage.uncached_input_tokens, Some(6));
        assert_eq!(normalized.raw, raw);
    }
}
