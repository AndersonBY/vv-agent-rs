use serde_json::Value;
use std::path::Path;
use std::sync::OnceLock;

use crate::types::Message;

pub fn resolve_model_token_limits(
    settings: &Value,
    backend: &str,
    model: &str,
) -> (Option<u64>, Option<u64>) {
    let backend = backend.trim();
    let model = model.trim();
    if backend.is_empty() || model.is_empty() {
        return (None, None);
    }
    let Ok(resolved) = crate::config::resolve_model_endpoint(settings, backend, model) else {
        return (None, None);
    };
    (resolved.context_length, resolved.max_output_tokens)
}

pub fn resolve_model_token_limits_from_file(
    path: impl AsRef<Path>,
    backend: &str,
    model: &str,
) -> (Option<u64>, Option<u64>) {
    let Ok(settings) = crate::config::load_llm_settings_from_file(path) else {
        return (None, None);
    };
    resolve_model_token_limits(&settings, backend, model)
}

pub fn compute_compaction_threshold(
    configured_threshold: u64,
    model_context_window: u64,
    reserved_output_tokens: u64,
    autocompact_buffer_tokens: u64,
) -> u64 {
    let derived = if model_context_window > 0 {
        model_context_window
            .saturating_sub(reserved_output_tokens)
            .saturating_sub(autocompact_buffer_tokens)
    } else {
        0
    };
    match (configured_threshold, derived) {
        (configured, derived) if configured > 0 && derived > 0 => configured.min(derived),
        (configured, _) if configured > 0 => configured,
        (_, derived) => derived,
    }
}

pub fn count_messages_tokens(messages: &[Message], model: &str) -> u64 {
    if messages.is_empty() {
        return 0;
    }
    let combined_text = messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let text_tokens = if combined_text.is_empty() {
        0
    } else {
        count_tokens(&combined_text, model)
    };
    let image_count = messages
        .iter()
        .filter(|message| message.image_url.is_some())
        .count() as u64;

    text_tokens + image_count * default_image_tokens(model)
}

pub trait TokenCountPayload {
    fn to_token_count_text(&self) -> String;
}

impl TokenCountPayload for str {
    fn to_token_count_text(&self) -> String {
        self.to_string()
    }
}

impl TokenCountPayload for String {
    fn to_token_count_text(&self) -> String {
        self.clone()
    }
}

impl TokenCountPayload for Value {
    fn to_token_count_text(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

pub fn count_tokens<T: TokenCountPayload + ?Sized>(payload: &T, model: &str) -> u64 {
    let text = payload.to_token_count_text();
    if text.is_empty() {
        return 0;
    }
    let direct_count = vv_llm::utilities::count_tokens(&text, model);
    if vv_llm_has_universal_model_dispatch() {
        if let Ok(count) = direct_count {
            if count > 0 {
                return count as u64;
            }
        }
    } else if let Some(count) = count_tokens_with_legacy_vv_llm(&text, model) {
        if count > 0 {
            return count as u64;
        }
    }
    estimate_tokens(&text, model)
}

pub fn estimate_tokens(text: &str, _model: &str) -> u64 {
    if text.is_empty() {
        return 0;
    }
    let mut cjk_chars = 0_u64;
    let mut other_chars = 0_u64;
    for ch in text.chars() {
        if is_cjk(ch) {
            cjk_chars += 1;
        } else {
            other_chars += 1;
        }
    }
    ((cjk_chars as f64 * 1.5) + (other_chars as f64 * 0.25))
        .floor()
        .max(1.0) as u64
}

fn default_image_tokens(model: &str) -> u64 {
    if model.to_ascii_lowercase().starts_with("moonshot") {
        1_024
    } else {
        765
    }
}

fn vv_llm_has_universal_model_dispatch() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        matches!(
            vv_llm::utilities::count_tokens(
                "antidisestablishmentarianism",
                "unknown-provider-model",
            ),
            Ok(6)
        )
    })
}

fn count_tokens_with_legacy_vv_llm(text: &str, model: &str) -> Option<usize> {
    let normalized_model = model.to_ascii_lowercase();
    if normalized_model.starts_with("abab") || normalized_model.starts_with("minimax") {
        return Some((text.chars().count() as f64 / 1.33) as usize);
    }

    // vv-llm 0.2.x exposes both BPEs but only dispatches a few model names.
    let tokenizer_model = if normalized_model == "gpt-3.5-turbo"
        || normalized_model.starts_with("moonshot")
        || normalized_model.starts_with("kimi")
        || normalized_model.starts_with("gemini")
        || normalized_model.starts_with("stepfun")
        || normalized_model.starts_with("glm")
    {
        "gpt-3.5-turbo"
    } else {
        "gpt-4o"
    };
    vv_llm::utilities::count_tokens(text, tokenizer_model).ok()
}

fn is_cjk(ch: char) -> bool {
    matches!(
        ch as u32,
        0x4E00..=0x9FFF | 0x3000..=0x303F | 0xFF00..=0xFFEF
    )
}
