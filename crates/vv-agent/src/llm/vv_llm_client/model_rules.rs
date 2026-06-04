use serde_json::Value;

const REASONING_CHAIN_PROVIDERS: &[&str] = &["deepseek", "minimax", "moonshot"];
const REASONING_CHAIN_MODEL_PREFIXES: &[&str] = &["deepseek-", "minimax-", "kimi-", "moonshot-"];
const CLAUDE_THINKING_MODELS: &[&str] = &[
    "claude-3-7-sonnet-thinking",
    "claude-opus-4-20250514-thinking",
    "claude-opus-4-1-20250805-thinking",
    "claude-sonnet-4-20250514-thinking",
    "claude-sonnet-4-5-20250929-thinking",
    "claude-opus-4-5-20251101-thinking",
    "claude-opus-4-6-thinking",
    "claude-sonnet-4-6-thinking",
];
const QWEN_THINKING_KEEP_SUFFIX_MODELS: &[&str] = &[
    "qwen3-next-80b-a3b-thinking",
    "qwen3-vl-235b-a22b-thinking",
    "qwen3-vl-32b-thinking",
    "qwen3-vl-30b-a3b-thinking",
    "qwen3-vl-8b-thinking",
];

#[derive(Debug, Clone, PartialEq)]
pub(super) struct ResolvedRequestOptions {
    pub(super) model: String,
    pub(super) temperature: Option<f32>,
    pub(super) max_tokens: Option<u32>,
    pub(super) extra_body: Value,
}

pub(super) fn resolve_request_options(
    backend: &str,
    endpoint_provider: &str,
    model: &str,
) -> ResolvedRequestOptions {
    let mut resolved_model = model.to_string();
    let mut normalized_model = resolved_model.to_ascii_lowercase();
    let mut temperature = None;
    let mut max_tokens = None;
    let mut extra_body = Value::Null;

    if uses_deepseek_reasoning_defaults(backend, endpoint_provider, &normalized_model) {
        temperature = Some(0.6);
    } else if CLAUDE_THINKING_MODELS
        .iter()
        .any(|candidate| normalized_model == *candidate)
    {
        resolved_model = remove_suffix_case_insensitive(&resolved_model, "-thinking");
        normalized_model = resolved_model.to_ascii_lowercase();
        temperature = Some(1.0);
        max_tokens = Some(20_000);
        extra_body = serde_json::json!({
            "thinking": {"type": "enabled", "budget_tokens": 16000}
        });
    }

    if matches!(normalized_model.as_str(), "o3-mini-high" | "o4-mini-high")
        || (normalized_model.starts_with("gpt-5") && normalized_model.ends_with("-high"))
    {
        resolved_model = remove_suffix_case_insensitive(&resolved_model, "-high");
        normalized_model = resolved_model.to_ascii_lowercase();
        extra_body = serde_json::json!({"reasoning_effort": "high"});
    }

    if normalized_model.starts_with("qwen3") {
        if normalized_model.ends_with("-thinking") {
            if !QWEN_THINKING_KEEP_SUFFIX_MODELS
                .iter()
                .any(|candidate| normalized_model == *candidate)
            {
                resolved_model = remove_suffix_case_insensitive(&resolved_model, "-thinking");
                normalized_model = resolved_model.to_ascii_lowercase();
            }
            extra_body = serde_json::json!({"enable_thinking": true});
        } else {
            extra_body = serde_json::json!({"enable_thinking": false});
        }
    }

    if (normalized_model.starts_with("glm-4.") || normalized_model.starts_with("glm-5"))
        && normalized_model.ends_with("-thinking")
    {
        resolved_model = remove_suffix_case_insensitive(&resolved_model, "-thinking");
        normalized_model = resolved_model.to_ascii_lowercase();
        extra_body = serde_json::json!({"thinking": {"type": "enabled"}});
    }

    if normalized_model.starts_with("gemini-2.5") {
        extra_body = serde_json::json!({
            "extra_body": {
                "google": {
                    "thinking_config": {
                        "thinkingBudget": -1,
                        "include_thoughts": true
                    }
                }
            }
        });
    }

    if normalized_model.starts_with("gemini-3") {
        temperature.get_or_insert(1.0);
        if normalized_model == "gemini-3-pro" || normalized_model == "gemini-3-flash" {
            resolved_model = format!("{resolved_model}-preview");
        }
        extra_body = serde_json::json!({
            "extra_body": {
                "google": {
                    "thinking_config": {
                        "thinkingLevel": "high",
                        "include_thoughts": true
                    }
                }
            }
        });
    }

    ResolvedRequestOptions {
        model: resolved_model,
        temperature,
        max_tokens,
        extra_body,
    }
}

fn is_reasoning_chain_provider(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    REASONING_CHAIN_PROVIDERS.iter().any(|provider| {
        normalized == *provider
            || normalized.starts_with(&format!("{provider}-"))
            || normalized.starts_with(&format!("{provider}_"))
    })
}

fn uses_deepseek_reasoning_defaults(backend: &str, endpoint_provider: &str, model: &str) -> bool {
    model.trim().to_ascii_lowercase().starts_with("deepseek-")
        || is_reasoning_chain_provider(backend)
            && backend.trim().to_ascii_lowercase().starts_with("deepseek")
        || is_reasoning_chain_provider(endpoint_provider)
            && endpoint_provider
                .trim()
                .to_ascii_lowercase()
                .starts_with("deepseek")
}

fn remove_suffix_case_insensitive(value: &str, suffix: &str) -> String {
    if value.to_ascii_lowercase().ends_with(suffix) {
        value[..value.len().saturating_sub(suffix.len())].to_string()
    } else {
        value.to_string()
    }
}

pub(super) fn should_use_stream(_model: &str) -> bool {
    true
}

pub(super) fn should_preserve_reasoning_chain(backend: &str, candidates: &[&str]) -> bool {
    if is_reasoning_chain_provider(backend) {
        return true;
    }
    candidates.iter().any(|candidate| {
        let normalized = candidate.trim().to_ascii_lowercase();
        !normalized.is_empty()
            && REASONING_CHAIN_MODEL_PREFIXES
                .iter()
                .any(|prefix| normalized.starts_with(prefix))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_use_stream_matches_model_rules() {
        assert!(should_use_stream("deepseek-v4-pro"));
        assert!(should_use_stream("MiniMax-M2.1"));
        assert!(should_use_stream("claude-sonnet-4-6-thinking"));
        assert!(should_use_stream("gpt-4o-mini"));
        assert!(should_use_stream("custom-enterprise-model"));
    }

    #[test]
    fn deepseek_prefix_defaults_new_models_to_reasoning_temperature() {
        let options = resolve_request_options("openai", "default", "deepseek-v5-pro");

        assert_eq!(options.temperature, Some(0.6));
    }

    #[test]
    fn deepseek_provider_defaults_aliases_to_reasoning_temperature() {
        let options = resolve_request_options("deepseek", "default", "enterprise-reasoner");

        assert_eq!(options.temperature, Some(0.6));
    }

    #[test]
    fn reasoning_chain_uses_provider_defaults() {
        assert!(should_preserve_reasoning_chain(
            "moonshot",
            &["enterprise-kimi"]
        ));
        assert!(should_preserve_reasoning_chain(
            "minimax",
            &["future-model"]
        ));
        assert!(should_preserve_reasoning_chain(
            "deepseek",
            &["custom-reasoner"]
        ));
        assert!(!should_preserve_reasoning_chain("openai", &["gpt-4o-mini"]));
    }
}
