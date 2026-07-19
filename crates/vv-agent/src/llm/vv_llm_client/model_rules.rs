use serde_json::Value;

use crate::model_settings::{ModelSettings, ToolChoice};

const REASONING_CHAIN_PROVIDERS: &[&str] = &["deepseek", "minimax", "moonshot"];
const REASONING_CHAIN_MODEL_PREFIXES: &[&str] = &["deepseek-", "minimax-", "kimi-", "moonshot-"];
const QWEN_THINKING_KEEP_SUFFIX_MODELS: &[&str] = &[
    "qwen3-next-80b-a3b-thinking",
    "qwen3-vl-235b-a22b-thinking",
    "qwen3-vl-32b-thinking",
    "qwen3-vl-30b-a3b-thinking",
    "qwen3-vl-8b-thinking",
];
const KIMI_K3_OMITTED_EXTRA_BODY_FIELDS: &[&str] = &[
    "enable_thinking",
    "frequency_penalty",
    "max_tokens",
    "n",
    "presence_penalty",
    "reasoning",
    "reasoning_effort",
    "temperature",
    "thinking",
    "top_p",
];

#[derive(Debug, Clone, PartialEq)]
pub(super) struct ResolvedRequestOptions {
    pub(super) model: String,
    pub(super) temperature: Option<f32>,
    pub(super) max_tokens: Option<u32>,
    pub(super) max_completion_tokens: Option<u32>,
    pub(super) tool_choice: Option<ToolChoice>,
    pub(super) timeout: Option<std::time::Duration>,
    pub(super) extra_body: Value,
}

pub(super) fn resolve_request_options(
    backend: &str,
    endpoint_provider: &str,
    model: &str,
    model_settings: Option<&ModelSettings>,
) -> ResolvedRequestOptions {
    let mut resolved_model = model.to_string();
    let mut normalized_model = resolved_model.to_ascii_lowercase();
    let mut temperature = None;
    let mut max_tokens = None;
    let mut max_completion_tokens = None;
    let mut tool_choice = None;
    let mut timeout = None;
    let mut extra_body = Value::Null;

    if uses_deepseek_model(backend, endpoint_provider, &normalized_model) {
        extra_body = serde_json::json!({
            "thinking": {"type": "enabled"},
            "reasoning_effort": "max"
        });
    } else if normalized_model.starts_with("claude") && normalized_model.ends_with("-thinking") {
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

    if let Some(settings) = model_settings {
        if settings.temperature.is_some() {
            temperature = settings.temperature.map(|value| value as f32);
        }
        if settings.max_tokens.is_some() {
            max_tokens = settings.max_tokens;
        }
        if let Some(choice) = settings.tool_choice.as_ref() {
            tool_choice = Some(choice.clone());
        }
        timeout = settings.timeout;

        let body = ensure_object(&mut extra_body);
        if let Some(top_p) = settings.top_p {
            body.insert("top_p".to_string(), serde_json::json!(top_p));
        }
        if let Some(parallel_tool_calls) = settings.parallel_tool_calls {
            body.insert(
                "parallel_tool_calls".to_string(),
                Value::Bool(parallel_tool_calls),
            );
        }
        if let Some(response_format) = settings.response_format.as_ref() {
            body.insert(
                "response_format".to_string(),
                serde_json::to_value(response_format).unwrap_or(Value::Null),
            );
        }
        if let Some(reasoning) = settings.reasoning.as_ref() {
            project_reasoning(body, reasoning);
        }
        body.extend(settings.extra_body.clone());
    }

    if normalized_model == "kimi-k3" {
        // Apply K3's fixed provider profile after public settings so callers
        // cannot reintroduce unsupported K2.x thinking or sampling fields.
        temperature = None;
        max_completion_tokens = max_tokens.take();
        let body = ensure_object(&mut extra_body);
        for field_name in KIMI_K3_OMITTED_EXTRA_BODY_FIELDS {
            body.remove(*field_name);
        }
        if max_completion_tokens.is_some() {
            body.remove("max_completion_tokens");
        }
        body.insert(
            "reasoning_effort".to_string(),
            Value::String("max".to_string()),
        );
    }

    ResolvedRequestOptions {
        model: resolved_model,
        temperature,
        max_tokens,
        max_completion_tokens,
        tool_choice,
        timeout,
        extra_body,
    }
}

fn ensure_object(value: &mut Value) -> &mut serde_json::Map<String, Value> {
    if !value.is_object() {
        *value = Value::Object(serde_json::Map::new());
    }
    value
        .as_object_mut()
        .expect("value was converted to object")
}

fn project_reasoning(target: &mut serde_json::Map<String, Value>, reasoning: &Value) {
    let Value::Object(reasoning) = reasoning else {
        target.insert("reasoning".to_string(), reasoning.clone());
        return;
    };
    let mut thinking = reasoning.clone();
    let effort = thinking
        .remove("effort")
        .or_else(|| thinking.remove("reasoning_effort"));
    if let Some(effort) = effort {
        target.insert("reasoning_effort".to_string(), effort);
    }
    if !thinking.is_empty() {
        target.insert("thinking".to_string(), Value::Object(thinking));
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

fn uses_deepseek_model(backend: &str, endpoint_provider: &str, model: &str) -> bool {
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
    fn deepseek_prefix_defaults_new_models_to_reasoning_profile() {
        let options = resolve_request_options("openai", "default", "deepseek-v5-pro", None);

        assert_eq!(options.temperature, None);
        assert_eq!(
            options.extra_body,
            serde_json::json!({
                "thinking": {"type": "enabled"},
                "reasoning_effort": "max"
            })
        );
    }

    #[test]
    fn deepseek_provider_defaults_aliases_to_reasoning_profile() {
        let options = resolve_request_options("deepseek", "default", "enterprise-reasoner", None);

        assert_eq!(options.temperature, None);
        assert_eq!(
            options.extra_body,
            serde_json::json!({
                "thinking": {"type": "enabled"},
                "reasoning_effort": "max"
            })
        );
    }

    #[test]
    fn kimi_k3_enforces_provider_profile_after_public_settings() {
        let settings = ModelSettings::builder()
            .temperature(0.3)
            .top_p(0.7)
            .max_tokens(4096)
            .reasoning(serde_json::json!({"effort": "low", "type": "enabled"}))
            .extra_body("temperature", serde_json::json!(0.4))
            .extra_body("top_p", serde_json::json!(0.8))
            .extra_body("n", serde_json::json!(2))
            .extra_body("presence_penalty", serde_json::json!(1))
            .extra_body("frequency_penalty", serde_json::json!(1))
            .extra_body("thinking", serde_json::json!({"type": "enabled"}))
            .extra_body("reasoning_effort", serde_json::json!("low"))
            .extra_body("max_tokens", serde_json::json!(1024))
            .extra_body("max_completion_tokens", serde_json::json!(2048))
            .extra_body("provider_option", serde_json::json!("kept"))
            .build();

        let options = resolve_request_options("moonshot", "moonshot", "kimi-k3", Some(&settings));

        assert_eq!(options.temperature, None);
        assert_eq!(options.max_tokens, None);
        assert_eq!(options.max_completion_tokens, Some(4096));
        assert_eq!(
            options.extra_body,
            serde_json::json!({
                "reasoning_effort": "max",
                "provider_option": "kept"
            })
        );
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

    #[test]
    fn named_tool_choice_is_projected_only_from_typed_model_settings() {
        let settings = ModelSettings::builder()
            .tool_choice(ToolChoice::Tool("lookup".to_string()))
            .build();

        let options = resolve_request_options("openai", "default", "demo", Some(&settings));

        assert_eq!(
            options.tool_choice,
            Some(ToolChoice::Tool("lookup".to_string()))
        );
    }
}
