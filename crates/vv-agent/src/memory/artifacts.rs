mod config;
mod content;
mod info;
mod persist;
mod render;

use crate::types::Message;

use self::content::{
    build_compacted_tool_content, is_compacted_tool_content, kept_tool_message_indices,
    should_compact_tool_message,
};
use self::info::build_tool_call_info;
use self::persist::persist_tool_content;

pub use self::config::ToolResultArtifactConfig;
pub use self::render::{render_persisted_artifacts_section, PersistedArtifact};

pub const TOOL_RESULT_COMPACT_MARKER: &str = "<Tool Result Compact>";

pub fn compact_tool_results(
    messages: &[Message],
    config: &ToolResultArtifactConfig,
    cycle_index: Option<u32>,
) -> (Vec<Message>, Vec<PersistedArtifact>, bool) {
    if config.compact_threshold == 0 {
        return (messages.to_vec(), Vec::new(), false);
    }
    let tool_info = build_tool_call_info(messages);
    let keep_indices = kept_tool_message_indices(messages, config.keep_last);

    let mut changed = false;
    let mut artifacts = Vec::new();
    let mut compacted = Vec::with_capacity(messages.len());
    for (index, message) in messages.iter().enumerate() {
        let info = message
            .tool_call_id
            .as_deref()
            .and_then(|tool_call_id| tool_info.get(tool_call_id));
        if !should_compact_tool_message(message, index, &keep_indices, config.compact_threshold) {
            if is_compacted_tool_content(&message.content) {
                if let Some(artifact) = persisted_artifact_from_compacted_content(
                    &message.content,
                    info.and_then(|item| item.tool_name.as_deref()),
                    info.and_then(|item| item.arguments.as_deref()),
                ) {
                    artifacts.push(artifact);
                }
            }
            compacted.push(message.clone());
            continue;
        }

        let artifact_path = persist_tool_content(
            &message.content,
            message.tool_call_id.as_deref(),
            config,
            cycle_index,
        );
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

fn persisted_artifact_from_compacted_content(
    content: &str,
    fallback_tool_name: Option<&str>,
    arguments: Option<&str>,
) -> Option<PersistedArtifact> {
    let mut path = None;
    let mut tool_name = fallback_tool_name.map(str::to_string);
    for line in content.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("artifact_path:") {
            let value = value.trim();
            if !value.is_empty() && value != "N/A" {
                path = Some(value.to_string());
            }
        } else if tool_name.is_none() {
            tool_name = line
                .strip_prefix("tool_name:")
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
        }
    }
    Some(PersistedArtifact {
        path: path?,
        tool_name,
        arguments: arguments.map(str::to_string),
    })
}
