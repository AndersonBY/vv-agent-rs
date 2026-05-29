use crate::types::MessageRole;

pub(super) fn normalize_excerpt(content: &str, limit: usize) -> String {
    let normalized = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.len() <= limit {
        normalized
    } else {
        format!("{}...", &normalized[..limit.saturating_sub(3)])
    }
}

pub(super) fn role_name(role: MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    }
}

pub(super) fn extract_between<'a>(
    text: &'a str,
    start_marker: &str,
    end_marker: &str,
) -> Option<&'a str> {
    let start = text.find(start_marker)?;
    let rest = &text[start + start_marker.len()..];
    let end = rest.find(end_marker)?;
    Some(rest[..end].trim())
}
