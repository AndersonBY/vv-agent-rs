use std::panic::{catch_unwind, AssertUnwindSafe};

use crate::memory::artifacts::{
    compact_tool_results, render_persisted_artifacts_section, ToolResultArtifactConfig,
};
use crate::memory::post_compact_restore::{restore_key_files, PostCompactRestoreConfig};
use crate::memory::summary::LocalSummary;
use crate::types::{Message, MessageRole};

use super::helpers::{extract_original_user_request, normalize_summary_output};
use super::normalization;
use super::prompts;
use super::MemoryManager;

impl MemoryManager {
    pub(super) fn compact_large_tool_results(
        &self,
        messages: &[Message],
        cycle_index: Option<u32>,
    ) -> (Vec<Message>, bool) {
        let (compacted, _artifacts, changed) = compact_tool_results(
            messages,
            &ToolResultArtifactConfig {
                workspace: self.config.workspace.clone(),
                artifact_dir: self.config.tool_result_artifact_dir.clone(),
                compact_threshold: self.config.tool_result_compact_threshold,
                keep_last: self.config.tool_result_keep_last,
                excerpt_head: self.config.tool_result_excerpt_head,
                excerpt_tail: self.config.tool_result_excerpt_tail,
            },
            cycle_index,
        );
        (compacted, changed)
    }

    pub(super) fn compress_memory(
        &self,
        messages: &[Message],
        cycle_index: Option<u32>,
    ) -> (Vec<Message>, bool) {
        let messages = self.strip_session_memory_context(messages);
        if messages.len() <= 2 {
            return (messages, false);
        }
        let system_message = messages
            .iter()
            .find(|message| message.role == MessageRole::System)
            .cloned();
        let (messages_for_summary, _normalized) = self.normalize_compaction_messages(&messages);
        let (messages_for_summary, artifacts, _compacted_tools) = compact_tool_results(
            &messages_for_summary,
            &ToolResultArtifactConfig {
                workspace: self.config.workspace.clone(),
                artifact_dir: self.config.tool_result_artifact_dir.clone(),
                compact_threshold: self.config.tool_result_compact_threshold,
                keep_last: self.config.tool_result_keep_last,
                excerpt_head: self.config.tool_result_excerpt_head,
                excerpt_tail: self.config.tool_result_excerpt_tail,
            },
            cycle_index,
        );
        let original_request = extract_original_user_request(&messages).unwrap_or_default();
        let summary_prompt = self.build_compress_memory_prompt(&messages_for_summary);
        let artifact_facts = artifacts
            .iter()
            .filter(|artifact| !artifact.path.is_empty())
            .map(|artifact| {
                format!(
                    "{} (tool={})",
                    artifact.path,
                    artifact.tool_name.as_deref().unwrap_or("unknown")
                )
            })
            .collect::<Vec<_>>();
        let mut compressed_memory =
            self.generate_summary(&summary_prompt, &messages_for_summary, artifact_facts);
        if let Some(summary_data) = parse_first_json_object(&compressed_memory) {
            let restored_context = restore_key_files(
                &summary_data,
                self.config.workspace.as_deref(),
                &PostCompactRestoreConfig {
                    token_model: self.config.model.clone(),
                    ..PostCompactRestoreConfig::default()
                },
            );
            if !restored_context.is_empty() {
                compressed_memory.push_str("\n\n");
                compressed_memory.push_str(&restored_context);
            }
        }
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

    fn build_compress_memory_prompt(&self, messages: &[Message]) -> String {
        prompts::build_compress_memory_prompt(
            &self.config.language,
            self.config.summary_event_limit,
            messages,
        )
    }

    fn generate_summary(
        &self,
        prompt: &str,
        messages: &[Message],
        key_facts: Vec<String>,
    ) -> String {
        if let Some(callback) = &self.config.summary_callback {
            let callback_result = catch_unwind(AssertUnwindSafe(|| {
                callback(
                    prompt,
                    self.config.summary_backend.as_deref(),
                    self.config.summary_model.as_deref(),
                )
            }));
            if let Ok(Some(summary)) = callback_result {
                let normalized = normalize_summary_output(&summary);
                if !normalized.trim().is_empty() {
                    return normalized;
                }
            }
        }
        LocalSummary::from_messages_with_key_facts(
            messages,
            self.config.summary_event_limit,
            key_facts,
        )
        .to_json_string()
    }

    fn normalize_compaction_messages(&self, messages: &[Message]) -> (Vec<Message>, bool) {
        normalization::normalize_compaction_messages(
            messages,
            self.config.tool_calls_keep_last,
            self.config.assistant_no_tool_keep_last,
        )
    }
}

fn parse_first_json_object(raw: &str) -> Option<serde_json::Value> {
    raw.char_indices()
        .filter(|(_, character)| *character == '{')
        .find_map(|(index, _)| {
            serde_json::Deserializer::from_str(&raw[index..])
                .into_iter::<serde_json::Value>()
                .next()
                .and_then(Result::ok)
                .filter(serde_json::Value::is_object)
        })
}
