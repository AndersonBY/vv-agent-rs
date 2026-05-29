use serde_json::Value;

use crate::types::{Message, MessageRole};

use super::text::extract_between;

pub(super) fn collect_original_user_messages(messages: &[Message]) -> Vec<String> {
    let mut collected = Vec::new();
    for message in messages.iter().skip(1) {
        if message.role != MessageRole::User {
            continue;
        }
        let content = message.content.trim();
        if content.is_empty() {
            continue;
        }
        let compressed_originals = extract_compressed_original_user_messages(content);
        if !compressed_originals.is_empty() {
            for original in compressed_originals {
                push_unique_original(&mut collected, original);
            }
            continue;
        }
        if content.contains("<Compressed Agent Memory>") {
            continue;
        }
        push_unique_original(
            &mut collected,
            extract_original_user_request(content)
                .unwrap_or(content)
                .to_string(),
        );
    }
    collected
}

fn extract_original_user_request(content: &str) -> Option<&str> {
    let start = content.find("<Original User Request>")?;
    let rest = &content[start + "<Original User Request>".len()..];
    let end = rest.find("</Original User Request>")?;
    Some(rest[..end].trim())
}

fn extract_compressed_original_user_messages(content: &str) -> Vec<String> {
    let Some(summary_block) = extract_between(
        content,
        "<Compressed Agent Memory>",
        "</Compressed Agent Memory>",
    ) else {
        return Vec::new();
    };
    summary_block
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with('{'))
        .and_then(|line| serde_json::from_str::<Value>(line).ok())
        .and_then(|value| {
            value
                .get("original_user_messages")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::trim)
                        .filter(|item| !item.is_empty())
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
        })
        .unwrap_or_default()
}

fn push_unique_original(collected: &mut Vec<String>, original: String) {
    if !original.is_empty() && !collected.iter().any(|known| known == &original) {
        collected.push(original);
    }
}
