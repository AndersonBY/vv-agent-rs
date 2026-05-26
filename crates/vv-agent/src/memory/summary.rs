use serde::Serialize;

use crate::types::{Message, MessageRole};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalSummary {
    pub summary_version: String,
    pub original_user_messages: Vec<String>,
    pub user_constraints: Vec<String>,
    pub decisions: Vec<String>,
    pub files_examined_or_modified: Vec<String>,
    pub errors_and_fixes: Vec<String>,
    pub progress: Vec<String>,
    pub key_facts: Vec<String>,
    pub open_issues: Vec<String>,
    pub current_work_state: String,
    pub next_steps: Vec<String>,
}

impl LocalSummary {
    pub fn from_messages(messages: &[Message], event_limit: usize) -> Self {
        Self {
            summary_version: "2.0".to_string(),
            original_user_messages: collect_original_user_messages(messages),
            user_constraints: Vec::new(),
            decisions: Vec::new(),
            files_examined_or_modified: Vec::new(),
            errors_and_fixes: collect_errors(messages),
            progress: build_progress_events(messages, event_limit),
            key_facts: Vec::new(),
            open_issues: Vec::new(),
            current_work_state: current_work_state(messages),
            next_steps: Vec::new(),
        }
    }

    pub fn to_json_string(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }
}

fn collect_original_user_messages(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .skip(1)
        .filter(|message| message.role == MessageRole::User)
        .filter_map(|message| {
            let content = message.content.trim();
            if content.is_empty() || content.contains("<Compressed Agent Memory>") {
                None
            } else {
                Some(
                    extract_original_user_request(content)
                        .unwrap_or(content)
                        .to_string(),
                )
            }
        })
        .collect()
}

fn build_progress_events(messages: &[Message], event_limit: usize) -> Vec<String> {
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

fn collect_errors(messages: &[Message]) -> Vec<String> {
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

fn current_work_state(messages: &[Message]) -> String {
    messages
        .iter()
        .rev()
        .filter(|message| matches!(message.role, MessageRole::Assistant | MessageRole::User))
        .map(|message| normalize_excerpt(&message.content, 240))
        .find(|content| !content.is_empty())
        .unwrap_or_default()
}

fn extract_original_user_request(content: &str) -> Option<&str> {
    let start = content.find("<Original User Request>")?;
    let rest = &content[start + "<Original User Request>".len()..];
    let end = rest.find("</Original User Request>")?;
    Some(rest[..end].trim())
}

fn normalize_excerpt(content: &str, limit: usize) -> String {
    let normalized = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.len() <= limit {
        normalized
    } else {
        format!("{}...", &normalized[..limit.saturating_sub(3)])
    }
}

fn role_name(role: MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    }
}
