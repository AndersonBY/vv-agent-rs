use crate::types::{Message, MessageRole};

use super::{ToolResultArtifactConfig, TOOL_RESULT_COMPACT_MARKER};

pub(crate) fn is_compacted_tool_content(content: &str) -> bool {
    content.starts_with(TOOL_RESULT_COMPACT_MARKER)
}

pub(crate) fn has_recovery_envelope(content: &str) -> bool {
    let Some((_, last_line)) = content.trim_end().rsplit_once('\n') else {
        return false;
    };
    serde_json::from_str::<serde_json::Value>(last_line)
        .ok()
        .and_then(|value| value.get("vv_agent_recovery").cloned())
        .is_some()
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

pub(crate) fn build_compacted_tool_content(
    content: &str,
    artifact_path: &str,
    tool_name: &str,
    config: &ToolResultArtifactConfig,
) -> String {
    let excerpt_source = content_without_recovery_envelope(content);
    let head = take_chars(excerpt_source, config.excerpt_head);
    let tail = take_tail_chars(excerpt_source, config.excerpt_tail);
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
    let excerpt = excerpt_parts.join("\n").trim().to_string();
    format!(
        "{TOOL_RESULT_COMPACT_MARKER}\ntool_name: {tool_name}\nartifact_path: {artifact_path}\nretrieval_hint: use read_file on artifact_path if needed\nexcerpt:\n{excerpt}\n</Tool Result Compact>"
    )
}

pub(super) fn content_without_recovery_envelope(content: &str) -> &str {
    let trimmed = content.trim_end();
    let Some((prefix, _)) = trimmed.rsplit_once('\n') else {
        return content;
    };
    if has_recovery_envelope(content) {
        prefix
    } else {
        content
    }
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

#[cfg(test)]
mod tests {
    use super::build_compacted_tool_content;
    use crate::memory::artifacts::ToolResultArtifactConfig;

    #[test]
    fn compact_marker_trims_excerpt_and_omits_trailing_recovery_envelope() {
        let recovery = r#"{"vv_agent_recovery":{"artifact":{"path":"hidden"},"truncated":true}}"#;
        let content = format!(" \n useful result \n{recovery}\n \t");
        let marker = build_compacted_tool_content(
            &content,
            ".vv-agent/artifacts/run/call.txt",
            "web_search",
            &ToolResultArtifactConfig {
                excerpt_head: 200,
                excerpt_tail: 200,
                ..ToolResultArtifactConfig::default()
            },
        );

        assert!(marker.contains("excerpt:\nuseful result\n</Tool Result Compact>"));
        assert!(!marker.contains("vv_agent_recovery"));
        assert!(!marker.contains("\"path\":\"hidden\""));
    }
}
