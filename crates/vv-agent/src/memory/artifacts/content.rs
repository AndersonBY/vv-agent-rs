use crate::memory::microcompact::is_microcompacted_tool_content;
use crate::types::{Message, MessageRole};

use super::{ToolResultArtifactConfig, TOOL_RESULT_COMPACT_MARKER};

pub(super) fn is_compacted_tool_content(content: &str) -> bool {
    content.starts_with(TOOL_RESULT_COMPACT_MARKER) || is_microcompacted_tool_content(content)
}

pub(super) fn kept_tool_message_indices(messages: &[Message], keep_last: usize) -> Vec<usize> {
    if keep_last == 0 {
        return Vec::new();
    }
    messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| (message.role == MessageRole::Tool).then_some(index))
        .rev()
        .take(keep_last)
        .collect()
}

pub(super) fn should_compact_tool_message(
    message: &Message,
    index: usize,
    keep_indices: &[usize],
    compact_threshold: usize,
) -> bool {
    message.role == MessageRole::Tool
        && !keep_indices.contains(&index)
        && message.content.len() > compact_threshold
        && !is_compacted_tool_content(&message.content)
}

pub(super) fn build_compacted_tool_content(
    content: &str,
    artifact_path: Option<&str>,
    tool_name: Option<&str>,
    config: &ToolResultArtifactConfig,
) -> String {
    let head = take_chars(content, config.excerpt_head);
    let tail = take_tail_chars(content, config.excerpt_tail);
    let mut excerpt_parts = Vec::new();
    if !head.is_empty() {
        excerpt_parts.push(head.clone());
    }
    if !tail.is_empty() && tail != head {
        if !excerpt_parts.is_empty() {
            excerpt_parts.push("...<snip>...".to_string());
        }
        excerpt_parts.push(tail);
    }
    let excerpt = excerpt_parts.join("\n");
    let artifact_line = artifact_path.unwrap_or("N/A");
    let tool_line = tool_name
        .map(|tool_name| format!("tool_name: {tool_name}\n"))
        .unwrap_or_default();
    let truncated_chars = content.len().saturating_sub(excerpt.len());
    format!(
        "{TOOL_RESULT_COMPACT_MARKER}\n{tool_line}artifact_path: {artifact_line}\ntotal_chars: {}\ntruncated_chars: {truncated_chars}\nretrieval_hint: use read_file on artifact_path if needed\nexcerpt:\n{excerpt}\n</Tool Result Compact>",
        content.len()
    )
}

fn take_chars(content: &str, count: usize) -> String {
    content.chars().take(count).collect()
}

fn take_tail_chars(content: &str, count: usize) -> String {
    if count == 0 {
        return String::new();
    }
    let chars = content.chars().collect::<Vec<_>>();
    let start = chars.len().saturating_sub(count);
    chars[start..].iter().collect()
}
