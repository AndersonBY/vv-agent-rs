use std::collections::BTreeSet;

mod compaction;
mod config;
mod emergency;
mod helpers;
mod limits;
mod microcompact;
mod normalization;
mod prompts;
mod session_context;
mod warnings;

use crate::memory::message_sanitizer::filter_empty_assistant_messages;
use crate::memory::session::SessionMemory;
use crate::memory::token_utils::count_messages_tokens;
use crate::types::{Message, MessageRole};

pub use config::{MemoryManagerConfig, SummaryCallback};

use helpers::compact_processed_image_messages;

const MEMORY_SUMMARY_NAME: &str = "memory_summary";

#[derive(Debug, Clone)]
pub struct MemoryManager {
    pub config: MemoryManagerConfig,
    session_memory: Option<SessionMemory>,
}

impl MemoryManager {
    pub fn new(mut config: MemoryManagerConfig) -> Self {
        let session_memory = config.session_memory.take();
        Self {
            config,
            session_memory,
        }
    }

    pub fn compact(&mut self, messages: &[Message], force: bool) -> (Vec<Message>, bool) {
        self.compact_for_cycle_with_usage(messages, 0, force, None, None)
    }

    pub fn compact_for_cycle(
        &mut self,
        messages: &[Message],
        cycle_index: u32,
        force: bool,
    ) -> (Vec<Message>, bool) {
        self.compact_for_cycle_with_usage(messages, cycle_index, force, None, None)
    }

    pub fn compact_for_cycle_with_usage(
        &mut self,
        messages: &[Message],
        cycle_index: u32,
        force: bool,
        total_tokens: Option<u64>,
        recent_tool_call_ids: Option<&BTreeSet<String>>,
    ) -> (Vec<Message>, bool) {
        if messages.is_empty() {
            return (Vec::new(), false);
        }

        let cleaned = self.remove_previous_summary(messages);
        let sanitized = filter_empty_assistant_messages(&cleaned);
        let changed_by_sanitize = sanitized.len() != messages.len()
            || sanitized
                .iter()
                .zip(messages.iter())
                .any(|(left, right)| left != right);
        let mut working_messages = self.apply_session_memory_context(&sanitized);
        let mut message_length =
            self.calculate_effective_length(&working_messages, total_tokens, recent_tool_call_ids);
        if let Some(session_memory) = self.session_memory.as_mut() {
            let text_messages = working_messages
                .iter()
                .filter(|message| {
                    !matches!(message.role, MessageRole::System | MessageRole::Tool)
                        && !message.content.trim().is_empty()
                })
                .count();
            if session_memory.should_extract(message_length, text_messages)
                && session_memory.extract(&working_messages, cycle_index as i32, message_length) > 0
            {
                working_messages = self.apply_session_memory_context(&sanitized);
                message_length =
                    self.calculate_effective_length(&working_messages, None, recent_tool_call_ids);
            }
        }
        if !force && message_length <= self.autocompact_threshold() {
            let (warned, warning_inserted) =
                self.maybe_append_memory_warning(&working_messages, message_length);
            if warning_inserted {
                return (warned, true);
            }
            if self.should_preemptive_microcompact(message_length) {
                let (microcompacted, cleared) =
                    self.microcompact_messages(&working_messages, cycle_index);
                if cleared > 0 {
                    return (microcompacted, true);
                }
            }
            return (working_messages, changed_by_sanitize);
        }
        let mut summary_source = self.strip_session_memory_context(&working_messages);
        if !force {
            let (microcompacted, cleared) =
                self.microcompact_messages(&working_messages, cycle_index);
            if cleared > 0 {
                if self.calculate_effective_length(&microcompacted, None, None)
                    <= self.autocompact_threshold()
                {
                    return (microcompacted, true);
                }
                summary_source = microcompacted;
            }
            let (image_compacted, image_changed) =
                compact_processed_image_messages(&summary_source);
            let (artifact_compacted, artifact_changed) =
                self.compact_large_tool_results(&image_compacted);
            if (image_changed || artifact_changed)
                && count_messages_tokens(&artifact_compacted, &self.config.model)
                    <= self.autocompact_threshold()
            {
                return (artifact_compacted, true);
            }
            if image_changed {
                summary_source = image_compacted;
            }
        }
        let (compacted, changed) = self.compress_memory(&summary_source);
        if changed {
            if let Some(session_memory) = self.session_memory.as_mut() {
                session_memory
                    .on_compaction(Some(count_messages_tokens(&compacted, &self.config.model)));
            }
        }
        (compacted, changed)
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
}
