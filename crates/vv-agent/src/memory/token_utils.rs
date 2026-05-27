use serde::Serialize;
use serde_json::Value;
use std::path::Path;

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
    let payload = messages
        .iter()
        .map(message_to_openai_value)
        .collect::<Vec<_>>();
    count_tokens(&serde_json::to_string(&payload).unwrap_or_default(), model)
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
    if vv_llm_tokenizer_supported(model) {
        if let Ok(count) = vv_llm::utilities::count_tokens(&text, model) {
            if count > 0 {
                return count as u64;
            }
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

fn vv_llm_tokenizer_supported(model: &str) -> bool {
    model == "gpt-3.5-turbo"
        || model.starts_with("gpt-4o")
        || model.starts_with("o1-")
        || model.starts_with("o3-")
}

fn is_cjk(ch: char) -> bool {
    matches!(
        ch as u32,
        0x4E00..=0x9FFF | 0x3000..=0x303F | 0xFF00..=0xFFEF
    )
}

fn message_to_openai_value(message: &Message) -> Value {
    #[derive(Serialize)]
    struct OpenAiMessage<'a> {
        role: &'static str,
        content: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_call_id: Option<&'a str>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        tool_calls: &'a Vec<crate::types::ToolCall>,
        #[serde(skip_serializing_if = "Option::is_none")]
        image_url: Option<&'a str>,
    }
    let role = match message.role {
        crate::types::MessageRole::System => "system",
        crate::types::MessageRole::User => "user",
        crate::types::MessageRole::Assistant => "assistant",
        crate::types::MessageRole::Tool => "tool",
    };
    serde_json::to_value(OpenAiMessage {
        role,
        content: &message.content,
        name: message.name.as_deref(),
        tool_call_id: message.tool_call_id.as_deref(),
        tool_calls: &message.tool_calls,
        image_url: message.image_url.as_deref(),
    })
    .unwrap_or(Value::Null)
}
