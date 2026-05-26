use crate::llm::LlmError;

pub(crate) const MAX_PROMPT_TOO_LONG_RETRIES: u32 = 3;

const PROMPT_TOO_LONG_PATTERNS: &[&str] = &[
    "prompt is too long",
    "prompt_too_long",
    "context_length_exceeded",
    "maximum context length",
    "request too large",
    "too many tokens",
];

pub(crate) fn is_prompt_too_long_error(error: &LlmError) -> bool {
    let text = error.to_string().to_ascii_lowercase();
    PROMPT_TOO_LONG_PATTERNS
        .iter()
        .any(|pattern| text.contains(pattern))
}
