use std::collections::BTreeMap;

use serde_json::Value;

use crate::types::{Message, MessageRole, ToolArguments};

use super::FileAction;

pub(super) fn collect_file_actions(messages: &[Message]) -> Vec<FileAction> {
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
        "write_file" | "edit_file" => Some("modified"),
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
        "edit_file" => format!("Modified {path}"),
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
