use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::types::{Message, MessageRole, ToolArguments};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileAction {
    pub path: String,
    pub action: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalSummary {
    pub summary_version: String,
    pub original_user_messages: Vec<String>,
    pub user_constraints: Vec<String>,
    pub decisions: Vec<String>,
    pub files_examined_or_modified: Vec<FileAction>,
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
            files_examined_or_modified: collect_file_actions(messages),
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

fn collect_file_actions(messages: &[Message]) -> Vec<FileAction> {
    let mut actions_by_path = BTreeMap::<String, FileAction>::new();
    let mut ordered_paths = Vec::<String>::new();
    for message in messages {
        if message.role != MessageRole::Assistant || message.tool_calls.is_empty() {
            continue;
        }
        for tool_call in &message.tool_calls {
            let Some(action) = tool_action(&tool_call.name) else {
                continue;
            };
            let Some(path) = extract_file_path_from_arguments(&tool_call.arguments) else {
                continue;
            };
            let summary = summarize_file_action(&tool_call.name, &path);
            if let Some(existing) = actions_by_path.get_mut(&path) {
                if action_priority(action) < action_priority(&existing.action) {
                    existing.action = action.to_string();
                }
                existing.summary = summary;
            } else {
                actions_by_path.insert(
                    path.clone(),
                    FileAction {
                        path: path.clone(),
                        action: action.to_string(),
                        summary,
                    },
                );
                ordered_paths.push(path);
            }
        }
    }
    ordered_paths
        .into_iter()
        .filter_map(|path| actions_by_path.remove(&path))
        .collect()
}

fn tool_action(tool_name: &str) -> Option<&'static str> {
    match tool_name {
        "read_file" | "file_info" => Some("read"),
        "write_file" | "file_str_replace" => Some("modified"),
        _ => None,
    }
}

fn extract_file_path_from_arguments(arguments: &ToolArguments) -> Option<String> {
    ["path", "file_path", "filepath", "target_file"]
        .iter()
        .filter_map(|key| arguments.get(*key))
        .find_map(value_to_trimmed_string)
}

fn value_to_trimmed_string(value: &Value) -> Option<String> {
    let value = value.as_str()?.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn summarize_file_action(tool_name: &str, path: &str) -> String {
    match tool_name {
        "read_file" => format!("Read {path}"),
        "file_info" => format!("Inspected {path}"),
        "write_file" => format!("Updated {path}"),
        "file_str_replace" => format!("Modified {path}"),
        _ => format!("Touched {path}"),
    }
}

fn action_priority(action: &str) -> u8 {
    match action {
        "modified" => 0,
        "created" => 1,
        "deleted" => 2,
        "read" => 3,
        _ => 99,
    }
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
