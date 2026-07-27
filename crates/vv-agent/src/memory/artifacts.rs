mod config;
mod content;
mod info;
mod persist;
mod render;

use std::sync::Arc;

use crate::types::Message;
use crate::workspace::{read_validated_text_artifact, WorkspaceBackend};

pub(crate) use self::content::{
    build_compacted_tool_content, has_recovery_envelope, is_compacted_tool_content,
};
use self::content::{kept_tool_message_indices, should_compact_tool_message};
use self::info::build_tool_call_info;
use self::persist::persist_tool_content;

pub use self::config::ToolResultArtifactConfig;
pub use self::render::{render_persisted_artifacts_section, PersistedArtifact};

pub const TOOL_RESULT_COMPACT_MARKER: &str = "<Tool Result Compact>";

pub fn compact_tool_results(
    messages: &[Message],
    config: &ToolResultArtifactConfig,
    backend: Option<&Arc<dyn WorkspaceBackend>>,
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
                if let Some(artifact) = persisted_artifact_from_compacted_message(
                    message,
                    info.and_then(|item| item.tool_name.as_deref()),
                    info.and_then(|item| item.arguments.as_deref()),
                ) {
                    artifacts.push(artifact);
                }
            }
            compacted.push(message.clone());
            continue;
        }

        let Some(backend) = backend else {
            compacted.push(message.clone());
            continue;
        };
        let (artifact, excerpt_source) = if let Some(artifact) = message.artifact_ref.as_ref() {
            let Ok(complete_content) = read_validated_text_artifact(backend.as_ref(), artifact)
            else {
                compacted.push(message.clone());
                continue;
            };
            (artifact.clone(), complete_content)
        } else {
            if has_recovery_envelope(&message.content) {
                compacted.push(message.clone());
                continue;
            }
            let Some(artifact) = persist_tool_content(
                &message.content,
                message.tool_call_id.as_deref(),
                backend,
                &config.artifact_namespace,
            ) else {
                compacted.push(message.clone());
                continue;
            };
            (artifact, message.content.clone())
        };
        let artifact_path = artifact.path.clone();
        if artifact_path.is_empty() {
            compacted.push(message.clone());
            continue;
        }
        let content = build_compacted_tool_content(
            &excerpt_source,
            &artifact_path,
            info.and_then(|item| item.tool_name.as_deref())
                .unwrap_or("unknown"),
            config,
        );
        let mut updated = message.clone();
        updated.content = content;
        updated.artifact_ref = Some(artifact);
        compacted.push(updated);
        artifacts.push(PersistedArtifact {
            path: artifact_path,
            tool_name: info.and_then(|item| item.tool_name.clone()),
            arguments: info.and_then(|item| item.arguments.clone()),
        });
        changed = true;
    }
    (compacted, artifacts, changed)
}

fn persisted_artifact_from_compacted_message(
    message: &Message,
    fallback_tool_name: Option<&str>,
    arguments: Option<&str>,
) -> Option<PersistedArtifact> {
    let mut tool_name = fallback_tool_name.map(str::to_string);
    for line in message.content.lines() {
        let line = line.trim();
        if tool_name.is_none() {
            tool_name = line
                .strip_prefix("tool_name:")
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
        }
    }
    Some(PersistedArtifact {
        path: message.artifact_ref.as_ref()?.path.clone(),
        tool_name,
        arguments: arguments.map(str::to_string),
    })
}
