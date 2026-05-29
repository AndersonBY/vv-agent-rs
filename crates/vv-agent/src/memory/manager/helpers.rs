use crate::memory::message_sanitizer::filter_empty_assistant_messages;
use crate::types::{Message, MessageRole};

pub(super) fn sanitize_empty_assistant_messages(messages: Vec<Message>) -> Vec<Message> {
    filter_empty_assistant_messages(&messages)
}

pub(super) fn normalize_summary_output(text: &str) -> String {
    let mut cleaned = strip_markdown_code_fence(text);
    let analysis_pattern =
        regex::Regex::new(r"(?is)<analysis>.*?</analysis>").expect("analysis regex");
    cleaned = analysis_pattern
        .replace_all(&cleaned, "")
        .trim()
        .to_string();
    let summary_pattern =
        regex::Regex::new(r"(?is)<summary>\s*(.*?)\s*</summary>").expect("summary regex");
    if let Some(captures) = summary_pattern.captures(&cleaned) {
        return captures
            .get(1)
            .map(|matched| matched.as_str().trim().to_string())
            .unwrap_or_default();
    }
    cleaned
}

fn strip_markdown_code_fence(text: &str) -> String {
    let cleaned = text.trim();
    if !cleaned.starts_with("```") {
        return cleaned.to_string();
    }
    let mut lines = cleaned.lines().collect::<Vec<_>>();
    if lines.len() < 2 {
        return cleaned.to_string();
    }
    lines.remove(0);
    if lines
        .last()
        .is_some_and(|line| line.trim().starts_with("```"))
    {
        lines.pop();
    }
    lines.join("\n").trim().to_string()
}

pub(super) fn extract_original_user_request(messages: &[Message]) -> Option<String> {
    messages
        .iter()
        .skip(1)
        .find(|message| message.role == MessageRole::User && !message.content.trim().is_empty())
        .map(|message| {
            let content = message.content.trim();
            if let Some(extracted) = extract_between(
                content,
                "<Original User Request>",
                "</Original User Request>",
            ) {
                extracted.to_string()
            } else {
                content.to_string()
            }
        })
}

fn extract_between<'a>(text: &'a str, start_marker: &str, end_marker: &str) -> Option<&'a str> {
    let start = text.find(start_marker)?;
    let rest = &text[start + start_marker.len()..];
    let end = rest.find(end_marker)?;
    Some(rest[..end].trim())
}

pub(super) fn adjust_start_for_tool_context(messages: &[Message], mut start_index: usize) -> usize {
    while start_index > 0 && start_index < messages.len() {
        let message = &messages[start_index];
        if message.role != MessageRole::Tool {
            break;
        }
        start_index -= 1;
    }
    start_index
}

pub(super) fn compact_processed_image_messages(messages: &[Message]) -> (Vec<Message>, bool) {
    let assistant_indices = messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| (message.role == MessageRole::Assistant).then_some(index))
        .collect::<Vec<_>>();
    if assistant_indices.is_empty() {
        return (messages.to_vec(), false);
    }

    let mut changed = false;
    let compacted = messages
        .iter()
        .enumerate()
        .map(|(index, message)| {
            if message.role == MessageRole::User
                && message.image_url.is_some()
                && assistant_indices
                    .iter()
                    .any(|assistant_index| *assistant_index > index)
            {
                changed = true;
                let mut updated = message.clone();
                updated.image_url = None;
                updated.content = format!("{} [image payload compacted]", updated.content)
                    .trim()
                    .to_string();
                updated
            } else {
                message.clone()
            }
        })
        .collect::<Vec<_>>();
    (compacted, changed)
}
