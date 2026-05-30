mod config;
mod content;
mod info;
mod persist;
mod render;

use crate::types::Message;

use self::content::{
    build_compacted_tool_content, kept_tool_message_indices, should_compact_tool_message,
};
use self::info::build_tool_call_info;
use self::persist::persist_tool_content;

pub use self::config::ToolResultArtifactConfig;
pub use self::render::{render_persisted_artifacts_section, PersistedArtifact};

pub const TOOL_RESULT_COMPACT_MARKER: &str = "<Tool Result Compact>";

pub fn compact_tool_results(
    messages: &[Message],
    config: &ToolResultArtifactConfig,
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
        if !should_compact_tool_message(message, index, &keep_indices, config.compact_threshold) {
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
