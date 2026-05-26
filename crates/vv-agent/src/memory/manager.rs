use std::path::PathBuf;

use crate::memory::artifacts::{
    compact_tool_results, render_persisted_artifacts_section, ToolResultArtifactConfig,
};
use crate::memory::summary::LocalSummary;
use crate::memory::token_utils::{compute_compaction_threshold, count_messages_tokens};
use crate::types::{Message, MessageRole};

const MEMORY_SUMMARY_NAME: &str = "memory_summary";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryManagerConfig {
    pub compact_threshold: u64,
    pub keep_recent_messages: usize,
    pub model: String,
    pub model_context_window: u64,
    pub reserved_output_tokens: u64,
    pub autocompact_buffer_tokens: u64,
    pub summary_event_limit: usize,
    pub tool_result_compact_threshold: usize,
    pub tool_result_keep_last: usize,
    pub tool_result_excerpt_head: usize,
    pub tool_result_excerpt_tail: usize,
    pub tool_result_artifact_dir: PathBuf,
    pub workspace: Option<PathBuf>,
}

impl Default for MemoryManagerConfig {
    fn default() -> Self {
        Self {
            compact_threshold: 128_000,
            keep_recent_messages: 10,
            model: String::new(),
            model_context_window: 200_000,
            reserved_output_tokens: 16_000,
            autocompact_buffer_tokens: 13_000,
            summary_event_limit: 40,
            tool_result_compact_threshold: 2_000,
            tool_result_keep_last: 3,
            tool_result_excerpt_head: 200,
            tool_result_excerpt_tail: 200,
            tool_result_artifact_dir: PathBuf::from(".memory/tool_results"),
            workspace: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MemoryManager {
    pub config: MemoryManagerConfig,
}

impl MemoryManager {
    pub fn new(config: MemoryManagerConfig) -> Self {
        Self { config }
    }

    pub fn autocompact_threshold(&self) -> u64 {
        compute_compaction_threshold(
            self.config.compact_threshold,
            self.config.model_context_window,
            self.config.reserved_output_tokens,
            self.config.autocompact_buffer_tokens,
        )
    }

    pub fn compact(&self, messages: &[Message], force: bool) -> (Vec<Message>, bool) {
        if messages.is_empty() {
            return (Vec::new(), false);
        }

        let cleaned = self.remove_previous_summary(messages);
        let sanitized = sanitize_empty_assistant_messages(cleaned);
        let changed_by_sanitize = sanitized.len() != messages.len()
            || sanitized
                .iter()
                .zip(messages.iter())
                .any(|(left, right)| left != right);
        let message_length = count_messages_tokens(&sanitized, &self.config.model);
        if !force && message_length <= self.autocompact_threshold() {
            return (sanitized, changed_by_sanitize);
        }
        self.compress_memory(&sanitized)
    }

    fn remove_previous_summary(&self, messages: &[Message]) -> Vec<Message> {
        messages
            .iter()
            .filter(|message| {
                !(message.role == MessageRole::System
                    && message.name.as_deref() == Some(MEMORY_SUMMARY_NAME))
            })
            .cloned()
            .collect()
    }

    fn compress_memory(&self, messages: &[Message]) -> (Vec<Message>, bool) {
        if messages.len() <= 2 {
            return (messages.to_vec(), false);
        }
        let system_message = messages
            .iter()
            .find(|message| message.role == MessageRole::System)
            .cloned();
        let (messages_for_summary, artifacts, _) = compact_tool_results(
            messages,
            &ToolResultArtifactConfig {
                workspace: self.config.workspace.clone(),
                artifact_dir: self.config.tool_result_artifact_dir.clone(),
                compact_threshold: self.config.tool_result_compact_threshold,
                keep_last: self.config.tool_result_keep_last,
                excerpt_head: self.config.tool_result_excerpt_head,
                excerpt_tail: self.config.tool_result_excerpt_tail,
            },
        );
        let original_request = extract_original_user_request(messages).unwrap_or_default();
        let summary =
            LocalSummary::from_messages(&messages_for_summary, self.config.summary_event_limit);
        let mut compressed_memory = summary.to_json_string();
        if let Some(artifact_section) = render_persisted_artifacts_section(&artifacts) {
            compressed_memory.push_str("\n\n");
            compressed_memory.push_str(&artifact_section);
        }

        let mut compacted = Vec::new();
        if let Some(system_message) = system_message {
            compacted.push(system_message);
        }
        compacted.push(Message::user(format!(
            "<Original User Request>\n{original_request}\n</Original User Request>\n\n<Compressed Agent Memory>\n{compressed_memory}\n</Compressed Agent Memory>"
        )));
        (compacted, true)
    }
}

fn sanitize_empty_assistant_messages(messages: Vec<Message>) -> Vec<Message> {
    messages
        .into_iter()
        .filter(|message| {
            message.role != MessageRole::Assistant
                || !message.content.trim().is_empty()
                || !message.tool_calls.is_empty()
                || message
                    .reasoning_content
                    .as_deref()
                    .is_some_and(|text| !text.trim().is_empty())
        })
        .collect()
}

fn extract_original_user_request(messages: &[Message]) -> Option<String> {
    messages
        .iter()
        .skip(1)
        .find(|message| message.role == MessageRole::User && !message.content.trim().is_empty())
        .map(|message| {
            let content = message.content.trim();
            if let Some(extracted) = extract_between(
                content,
                "<Original User Request>",
                "</Original User Request>",
            ) {
                extracted.to_string()
            } else {
                content.to_string()
            }
        })
}

fn extract_between<'a>(text: &'a str, start_marker: &str, end_marker: &str) -> Option<&'a str> {
    let start = text.find(start_marker)?;
    let rest = &text[start + start_marker.len()..];
    let end = rest.find(end_marker)?;
    Some(rest[..end].trim())
}
