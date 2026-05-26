use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::types::{Message, MessageRole};

pub const TOOL_RESULT_COMPACT_MARKER: &str = "<Tool Result Compact>";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResultArtifactConfig {
    pub workspace: Option<PathBuf>,
    pub artifact_dir: PathBuf,
    pub compact_threshold: usize,
    pub keep_last: usize,
    pub excerpt_head: usize,
    pub excerpt_tail: usize,
}

impl Default for ToolResultArtifactConfig {
    fn default() -> Self {
        Self {
            workspace: None,
            artifact_dir: PathBuf::from(".memory/tool_results"),
            compact_threshold: 2_000,
            keep_last: 3,
            excerpt_head: 200,
            excerpt_tail: 200,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedArtifact {
    pub path: String,
    pub tool_name: Option<String>,
    pub arguments: Option<String>,
}

pub fn compact_tool_results(
    messages: &[Message],
    config: &ToolResultArtifactConfig,
) -> (Vec<Message>, Vec<PersistedArtifact>, bool) {
    if config.compact_threshold == 0 {
        return (messages.to_vec(), Vec::new(), false);
    }
    let tool_info = build_tool_call_info(messages);
    let tool_indices = messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| (message.role == MessageRole::Tool).then_some(index))
        .collect::<Vec<_>>();
    let keep_indices = if config.keep_last == 0 {
        Vec::new()
    } else {
        tool_indices
            .iter()
            .rev()
            .take(config.keep_last)
            .copied()
            .collect::<Vec<_>>()
    };

    let mut changed = false;
    let mut artifacts = Vec::new();
    let mut compacted = Vec::with_capacity(messages.len());
    for (index, message) in messages.iter().enumerate() {
        if message.role != MessageRole::Tool
            || keep_indices.contains(&index)
            || message.content.len() <= config.compact_threshold
            || is_compacted_tool_content(&message.content)
        {
            compacted.push(message.clone());
            continue;
        }

        let info = message
            .tool_call_id
            .as_deref()
            .and_then(|tool_call_id| tool_info.get(tool_call_id));
        let artifact_path =
            persist_tool_content(&message.content, message.tool_call_id.as_deref(), config);
        let content = build_compacted_tool_content(
            &message.content,
            artifact_path.as_deref(),
            info.and_then(|item| item.tool_name.as_deref()),
            config,
        );
        let mut updated = message.clone();
        updated.content = content;
        compacted.push(updated);
        if let Some(path) = artifact_path {
            artifacts.push(PersistedArtifact {
                path,
                tool_name: info.and_then(|item| item.tool_name.clone()),
                arguments: info.and_then(|item| item.arguments.clone()),
            });
        }
        changed = true;
    }
    (compacted, artifacts, changed)
}

pub fn render_persisted_artifacts_section(artifacts: &[PersistedArtifact]) -> Option<String> {
    if artifacts.is_empty() {
        return None;
    }
    let mut lines = vec!["<Persisted Artifacts>".to_string()];
    for artifact in artifacts {
        let tool = artifact.tool_name.as_deref().unwrap_or("unknown");
        let arguments = artifact.arguments.as_deref().unwrap_or("");
        let hint = "retrieval_hint: use read_file on artifact_path if needed";
        if arguments.is_empty() {
            lines.push(format!("- {} (tool: {tool}, {hint})", artifact.path));
        } else {
            lines.push(format!(
                "- {} (tool: {tool}, arguments: {arguments}, {hint})",
                artifact.path
            ));
        }
    }
    lines.push("</Persisted Artifacts>".to_string());
    Some(lines.join("\n"))
}

pub fn is_compacted_tool_content(content: &str) -> bool {
    content.starts_with(TOOL_RESULT_COMPACT_MARKER)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolCallInfo {
    tool_name: Option<String>,
    arguments: Option<String>,
}

fn build_tool_call_info(messages: &[Message]) -> BTreeMap<String, ToolCallInfo> {
    let mut info = BTreeMap::new();
    for message in messages {
        if message.role != MessageRole::Assistant {
            continue;
        }
        for tool_call in &message.tool_calls {
            let arguments = serde_json::to_string(&tool_call.arguments).ok();
            info.insert(
                tool_call.id.clone(),
                ToolCallInfo {
                    tool_name: Some(tool_call.name.clone()),
                    arguments,
                },
            );
        }
    }
    info
}

fn persist_tool_content(
    content: &str,
    tool_call_id: Option<&str>,
    config: &ToolResultArtifactConfig,
) -> Option<String> {
    let workspace = config.workspace.as_ref()?;
    let artifact_path = build_tool_artifact_path(tool_call_id, &config.artifact_dir);
    let target = workspace.join(&artifact_path);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    std::fs::write(&target, content).ok()?;
    Some(path_to_string(&artifact_path))
}

fn build_tool_artifact_path(tool_call_id: Option<&str>, artifact_dir: &Path) -> PathBuf {
    let safe_id = sanitize_tool_call_id(tool_call_id.unwrap_or("tool_result"));
    artifact_dir.join(format!("{safe_id}.txt"))
}

fn sanitize_tool_call_id(value: &str) -> String {
    let safe = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if safe.trim().is_empty() {
        "tool_result".to_string()
    } else {
        safe
    }
}

fn build_compacted_tool_content(
    content: &str,
    artifact_path: Option<&str>,
    tool_name: Option<&str>,
    config: &ToolResultArtifactConfig,
) -> String {
    let head = take_chars(content, config.excerpt_head);
    let tail = take_tail_chars(content, config.excerpt_tail);
    let mut excerpt_parts = Vec::new();
    if !head.is_empty() {
        excerpt_parts.push(head);
    }
    if !tail.is_empty() && tail != excerpt_parts.first().cloned().unwrap_or_default() {
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

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
