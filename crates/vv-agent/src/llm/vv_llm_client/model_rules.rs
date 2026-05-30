use serde_json::Value;

const STREAM_MODEL_PREFIXES: &[&str] = &[
    "qwen3", "claude", "gemini", "kimi", "glm-4.", "glm-5", "gpt-5", "minimax",
];
const STREAM_MODEL_EXACT: &[&str] = &[
    "deepseek-reasoner",
    "deepseek-r1-tools",
    "deepseek-v4-flash",
    "deepseek-v4-pro",
];
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
const REASONING_CHAIN_MODELS: &[&str] = &[
    "deepseek-reasoner",
    "deepseek-r1-tools",
    "deepseek-v4-flash",
    "deepseek-v4-pro",
    "kimi-k2.5",
    "kimi-k2.6",
    "minimax-m2.1",
    "minimax-m2.1-lightning",
    "minimax-m2.1-highspeed",
    "minimax-m2.5",
    "minimax-m2.5-highspeed",
    "minimax-m2.7",
    "minimax-m2.7-highspeed",
];
const REASONING_CHAIN_PREFIXES: &[&str] = &["deepseek-", "minimax-m2."];

#[derive(Debug, Clone, PartialEq)]
pub(super) struct ResolvedRequestOptions {
    pub(super) model: String,
    pub(super) temperature: Option<f32>,
    pub(super) max_tokens: Option<u32>,
    pub(super) extra_body: Value,
}

pub(super) fn resolve_request_options(model: &str) -> ResolvedRequestOptions {
    let mut resolved_model = model.to_string();
    let mut normalized_model = resolved_model.to_ascii_lowercase();
    let mut temperature = None;
    let mut max_tokens = None;
    let mut extra_body = Value::Null;

    if STREAM_MODEL_EXACT
        .iter()
        .any(|candidate| normalized_model == *candidate)
    {
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

fn remove_suffix_case_insensitive(value: &str, suffix: &str) -> String {
    if value.to_ascii_lowercase().ends_with(suffix) {
        value[..value.len().saturating_sub(suffix.len())].to_string()
    } else {
        value.to_string()
    }
}

pub(super) fn should_use_stream(model: &str) -> bool {
    let normalized = model.trim().to_ascii_lowercase();
    STREAM_MODEL_EXACT
        .iter()
        .any(|candidate| normalized == *candidate)
        || STREAM_MODEL_PREFIXES
            .iter()
            .any(|prefix| normalized.starts_with(prefix))
}

pub(super) fn should_preserve_reasoning_chain(candidates: &[&str]) -> bool {
    candidates.iter().any(|candidate| {
        let normalized = candidate.trim().to_ascii_lowercase();
        !normalized.is_empty()
            && (REASONING_CHAIN_MODELS
                .iter()
                .any(|model| normalized == *model)
                || REASONING_CHAIN_PREFIXES
                    .iter()
                    .any(|prefix| normalized.starts_with(prefix)))
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
        assert!(!should_use_stream("gpt-4o-mini"));
    }
}
