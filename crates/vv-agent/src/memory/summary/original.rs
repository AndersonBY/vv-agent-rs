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
        if let Some(original) = extract_original_user_request(content) {
            push_unique_original(&mut collected, original.to_string());
        }
        let compressed_originals = extract_compressed_original_user_messages(content);
        for original in compressed_originals {
            push_unique_original(&mut collected, original);
        }
        if content.contains("<Compressed Agent Memory>") {
            continue;
        }
        if extract_original_user_request(content).is_none() {
            push_unique_original(&mut collected, content.to_string());
        }
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
    extract_first_json_object(summary_block)
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

fn extract_first_json_object(raw: &str) -> Option<Value> {
    raw.char_indices()
        .filter(|(_, character)| *character == '{')
        .find_map(|(index, _)| {
            serde_json::Deserializer::from_str(&raw[index..])
                .into_iter::<Value>()
                .next()
                .and_then(Result::ok)
                .filter(Value::is_object)
        })
}

fn push_unique_original(collected: &mut Vec<String>, original: String) {
    if !original.is_empty() && !collected.iter().any(|known| known == &original) {
        collected.push(original);
    }
}
