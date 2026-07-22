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

use crate::events::{MemoryCompactMode, ReservedOutputSource};
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
    model_max_output_tokens: Option<u64>,
    reserved_output_source: ReservedOutputSource,
}

#[derive(Debug)]
pub(crate) struct MemoryCompactionOutcome {
    pub(crate) messages: Vec<Message>,
    pub(crate) changed: bool,
    pub(crate) mode: MemoryCompactMode,
}

impl MemoryManager {
    pub fn new(mut config: MemoryManagerConfig) -> Self {
        let session_memory = config.session_memory.take();
        Self {
            config,
            session_memory,
            model_max_output_tokens: None,
            reserved_output_source: ReservedOutputSource::FrameworkFallback,
        }
    }

    pub(crate) fn with_capacity_observation(
        mut self,
        model_max_output_tokens: Option<u64>,
        reserved_output_source: ReservedOutputSource,
    ) -> Self {
        self.model_max_output_tokens = model_max_output_tokens;
        self.reserved_output_source = reserved_output_source;
        self
    }

    pub(crate) fn model_max_output_tokens(&self) -> Option<u64> {
        self.model_max_output_tokens
    }

    pub(crate) fn reserved_output_source(&self) -> ReservedOutputSource {
        self.reserved_output_source
    }

    pub fn compact(&mut self, messages: &[Message], force: bool) -> (Vec<Message>, bool) {
        self.compact_for_cycle_with_usage_inner(messages, 0, None, force, None, None)
            .into_tuple()
    }

    pub fn compact_for_cycle(
        &mut self,
        messages: &[Message],
        cycle_index: u32,
        force: bool,
    ) -> (Vec<Message>, bool) {
        self.compact_for_cycle_with_usage_inner(
            messages,
            cycle_index,
            Some(cycle_index),
            force,
            None,
            None,
        )
        .into_tuple()
    }

    pub fn compact_for_cycle_with_usage(
        &mut self,
        messages: &[Message],
        cycle_index: u32,
        force: bool,
        total_tokens: Option<u64>,
        recent_tool_call_ids: Option<&BTreeSet<String>>,
    ) -> (Vec<Message>, bool) {
        self.compact_for_cycle_with_usage_inner(
            messages,
            cycle_index,
            Some(cycle_index),
            force,
            total_tokens,
            recent_tool_call_ids,
        )
        .into_tuple()
    }

    pub(crate) fn compact_for_cycle_with_usage_observed(
        &mut self,
        messages: &[Message],
        cycle_index: u32,
        force: bool,
        total_tokens: Option<u64>,
        recent_tool_call_ids: Option<&BTreeSet<String>>,
    ) -> MemoryCompactionOutcome {
        self.compact_for_cycle_with_usage_inner(
            messages,
            cycle_index,
            Some(cycle_index),
            force,
            total_tokens,
            recent_tool_call_ids,
        )
    }

    fn compact_for_cycle_with_usage_inner(
        &mut self,
        messages: &[Message],
        cycle_index: u32,
        artifact_cycle_index: Option<u32>,
        force: bool,
        total_tokens: Option<u64>,
        recent_tool_call_ids: Option<&BTreeSet<String>>,
    ) -> MemoryCompactionOutcome {
        if messages.is_empty() {
            return MemoryCompactionOutcome::new(
                messages,
                Vec::new(),
                MemoryCompactMode::None,
                false,
            );
        }

        let cleaned = self.remove_previous_summary(messages);
        let sanitized = filter_empty_assistant_messages(&cleaned);
        let changed_by_sanitize = sanitized != messages;
        let mut changed = changed_by_sanitize;
        let mut mode = if changed_by_sanitize {
            MemoryCompactMode::Structural
        } else {
            MemoryCompactMode::None
        };
        let mut working_messages = self.apply_session_memory_context(&sanitized);
        if working_messages != sanitized {
            mode = mode.max(MemoryCompactMode::Structural);
        }
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
                let before_refresh = working_messages;
                working_messages = self.apply_session_memory_context(&sanitized);
                if working_messages != before_refresh {
                    mode = mode.max(MemoryCompactMode::Structural);
                }
                message_length =
                    self.calculate_effective_length(&working_messages, None, recent_tool_call_ids);
            }
        }
        if !force && self.should_preemptive_microcompact(message_length) {
            let (microcompacted, cleared) =
                self.microcompact_messages(&working_messages, cycle_index);
            if cleared > 0 {
                working_messages = microcompacted;
                mode = mode.max(MemoryCompactMode::Micro);
                changed = true;
                message_length = self.calculate_effective_length(&working_messages, None, None);
            }
        }
        if !force && message_length <= self.autocompact_threshold() {
            let (warned, warning_inserted) =
                self.maybe_append_memory_warning(&working_messages, message_length);
            if warning_inserted {
                mode = mode.max(MemoryCompactMode::Structural);
                changed = true;
            }
            return MemoryCompactionOutcome::new(messages, warned, mode, changed);
        }
        let mut summary_source = self.strip_session_memory_context(&working_messages);
        if summary_source != working_messages {
            mode = mode.max(MemoryCompactMode::Structural);
        }
        if !force {
            let (image_compacted, image_changed) =
                compact_processed_image_messages(&summary_source);
            let (artifact_compacted, artifact_changed) =
                self.compact_large_tool_results(&image_compacted, artifact_cycle_index);
            if (image_changed || artifact_changed)
                && count_messages_tokens(&artifact_compacted, &self.config.model)
                    <= self.autocompact_threshold()
            {
                return MemoryCompactionOutcome::new(
                    messages,
                    artifact_compacted,
                    mode.max(MemoryCompactMode::Structural),
                    true,
                );
            }
            if image_changed || artifact_changed {
                mode = mode.max(MemoryCompactMode::Structural);
                summary_source = artifact_compacted;
            }
        }
        let (compacted, summary_changed) =
            self.compress_memory(&summary_source, artifact_cycle_index);
        if summary_changed {
            mode = mode.max(MemoryCompactMode::Summary);
            let post_compaction_tokens = count_messages_tokens(
                &self.apply_session_memory_context(&compacted),
                &self.config.model,
            );
            if let Some(session_memory) = self.session_memory.as_mut() {
                session_memory.on_compaction(Some(post_compaction_tokens));
            }
        }
        MemoryCompactionOutcome::new(messages, compacted, mode, changed || summary_changed)
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

impl MemoryCompactionOutcome {
    fn new(
        original: &[Message],
        messages: Vec<Message>,
        mode: MemoryCompactMode,
        changed: bool,
    ) -> Self {
        let content_changed = messages != original;
        let mode = if !content_changed {
            MemoryCompactMode::None
        } else if mode == MemoryCompactMode::None {
            MemoryCompactMode::Structural
        } else {
            mode
        };
        Self {
            messages,
            changed,
            mode,
        }
    }

    fn into_tuple(self) -> (Vec<Message>, bool) {
        (self.messages, self.changed)
    }
}
