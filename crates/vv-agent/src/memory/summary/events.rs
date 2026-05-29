use crate::types::{Message, MessageRole};

use super::text::{normalize_excerpt, role_name};

pub(super) fn build_progress_events(messages: &[Message], event_limit: usize) -> Vec<String> {
    let limit = event_limit.max(1);
    let mut events = Vec::new();
    for (index, message) in messages.iter().skip(2).take(limit).enumerate() {
        let mut content = normalize_excerpt(&message.content, 160);
        if content.is_empty() && !message.tool_calls.is_empty() {
            let tool_names = message
                .tool_calls
                .iter()
                .map(|call| call.name.as_str())
                .collect::<Vec<_>>()
                .join(",");
            content = format!("tool_calls={tool_names}");
        }
        events.push(format!(
            "{:02}. {}: {}",
            index + 1,
            role_name(message.role),
            content
        ));
    }
    if messages.len().saturating_sub(2) > limit {
        events.push(format!(
            "... {} more messages omitted ...",
            messages.len().saturating_sub(2 + limit)
        ));
    }
    events
}

pub(super) fn collect_errors(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter(|message| message.role == MessageRole::Tool)
        .filter(|message| {
            let lowered = message.content.to_ascii_lowercase();
            ["error", "exception", "traceback", "failed"]
                .iter()
                .any(|needle| lowered.contains(needle))
        })
        .take(5)
        .map(|message| normalize_excerpt(&message.content, 240))
        .collect()
}

pub(super) fn current_work_state(messages: &[Message]) -> String {
    messages
        .iter()
        .rev()
        .filter(|message| matches!(message.role, MessageRole::Assistant | MessageRole::User))
        .map(|message| normalize_excerpt(&message.content, 240))
        .find(|content| !content.is_empty())
        .unwrap_or_default()
}
