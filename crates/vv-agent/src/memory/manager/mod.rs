use std::collections::BTreeSet;

mod compaction;
mod config;
mod helpers;
mod normalization;
mod prompts;

use crate::memory::message_sanitizer::filter_empty_assistant_messages;
use crate::memory::microcompact::{microcompact, MicrocompactConfig};
use crate::memory::session::SessionMemory;
use crate::memory::token_utils::{compute_compaction_threshold, count_messages_tokens};
use crate::types::{Message, MessageRole};

pub use config::{MemoryManagerConfig, SummaryCallback};

use helpers::{
    adjust_start_for_tool_context, compact_processed_image_messages,
    sanitize_empty_assistant_messages,
};

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

    pub fn autocompact_threshold(&self) -> u64 {
        compute_compaction_threshold(
            self.config.compact_threshold,
            self.config.model_context_window,
            self.config.reserved_output_tokens,
            self.config.autocompact_buffer_tokens,
        )
    }

    pub fn effective_context_window(&self) -> u64 {
        self.config
            .model_context_window
            .saturating_sub(self.config.reserved_output_tokens)
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

    pub fn should_preemptive_microcompact(&self, message_length: u64) -> bool {
        let threshold = self.microcompact_trigger_threshold();
        threshold > 0 && message_length > threshold
    }

    pub fn estimate_memory_usage_percentage(
        &self,
        messages: &[Message],
        total_tokens: Option<u64>,
        recent_tool_call_ids: Option<&BTreeSet<String>>,
    ) -> u64 {
        let threshold = self.autocompact_threshold();
        if threshold == 0 {
            return 0;
        }
        let used_tokens =
            self.calculate_effective_length(messages, total_tokens, recent_tool_call_ids);
        (used_tokens.saturating_mul(100)) / threshold
    }

    pub fn warning_threshold(&self) -> u64 {
        let threshold = self.autocompact_threshold();
        if threshold == 0 {
            return 0;
        }
        (threshold * u64::from(self.config.warning_threshold_percentage)) / 100
    }

    pub fn microcompact_messages(
        &self,
        messages: &[Message],
        cycle_index: u32,
    ) -> (Vec<Message>, usize) {
        microcompact(
            messages,
            cycle_index,
            &MicrocompactConfig {
                trigger_ratio: self.config.microcompact_trigger_ratio,
                keep_recent_cycles: self.config.microcompact_keep_recent_cycles,
                min_result_length: self.config.microcompact_min_result_length,
                compactable_tools: self.config.microcompact_compactable_tools.clone(),
            },
        )
    }

    pub fn microcompact_trigger_threshold(&self) -> u64 {
        let ratio = self.config.microcompact_trigger_ratio.clamp(0.0, 1.0);
        (self.autocompact_threshold() as f64 * ratio).floor() as u64
    }

    fn calculate_effective_length(
        &self,
        messages: &[Message],
        total_tokens: Option<u64>,
        recent_tool_call_ids: Option<&BTreeSet<String>>,
    ) -> u64 {
        if let Some(total_tokens) = total_tokens.filter(|tokens| *tokens > 0) {
            return total_tokens
                + self.estimate_recent_tool_message_length(messages, recent_tool_call_ids);
        }
        count_messages_tokens(messages, &self.config.model)
    }

    fn estimate_recent_tool_message_length(
        &self,
        messages: &[Message],
        recent_tool_call_ids: Option<&BTreeSet<String>>,
    ) -> u64 {
        let Some(recent_tool_call_ids) = recent_tool_call_ids.filter(|ids| !ids.is_empty()) else {
            return 0;
        };
        let tool_messages = messages
            .iter()
            .filter(|message| {
                message.role == MessageRole::Tool
                    && message
                        .tool_call_id
                        .as_ref()
                        .is_some_and(|tool_call_id| recent_tool_call_ids.contains(tool_call_id))
            })
            .cloned()
            .collect::<Vec<_>>();
        count_messages_tokens(&tool_messages, &self.config.model)
    }

    fn maybe_append_memory_warning(
        &self,
        messages: &[Message],
        message_length: u64,
    ) -> (Vec<Message>, bool) {
        if !self.config.include_memory_warning || self.autocompact_threshold() == 0 {
            return (messages.to_vec(), false);
        }
        if message_length < self.warning_threshold() {
            return (messages.to_vec(), false);
        }
        let warning_text = self.memory_warning_text();
        if messages.iter().rev().take(10).any(|message| {
            message.role == MessageRole::User && message.content.contains(&warning_text)
        }) {
            return (messages.to_vec(), false);
        }
        let mut warned = messages.to_vec();
        warned.push(Message::user(warning_text));
        (warned, true)
    }

    fn memory_warning_text(&self) -> String {
        prompts::memory_warning_text(
            &self.config.language,
            self.config.warning_threshold_percentage,
        )
    }

    pub fn emergency_compact(&self, messages: &[Message], drop_ratio: f64) -> Vec<Message> {
        if messages.len() <= 2 {
            return messages.to_vec();
        }

        let (system_message, non_system) = if messages
            .first()
            .is_some_and(|message| message.role == MessageRole::System)
        {
            (messages.first().cloned(), &messages[1..])
        } else {
            (None, messages)
        };
        if non_system.is_empty() {
            return system_message.into_iter().collect();
        }

        let keep_count = self.config.keep_recent_messages.max(1);
        let clamped_ratio = drop_ratio.clamp(0.0, 0.95);
        let max_droppable = non_system.len().saturating_sub(keep_count);
        let drop_count = if max_droppable == 0 {
            0
        } else {
            ((non_system.len() as f64 * clamped_ratio).floor() as usize)
                .max(1)
                .min(max_droppable)
        };
        let mut start_index = drop_count.min(non_system.len());
        if non_system.len().saturating_sub(start_index) < keep_count {
            start_index = non_system.len().saturating_sub(keep_count);
        }
        start_index = adjust_start_for_tool_context(non_system, start_index);

        let mut compacted = Vec::new();
        if let Some(system_message) = system_message {
            compacted.push(system_message);
        }
        compacted.extend_from_slice(&non_system[start_index..]);
        sanitize_empty_assistant_messages(compacted)
    }

    pub fn session_memory(&self) -> Option<&SessionMemory> {
        self.session_memory.as_ref()
    }

    pub fn session_memory_mut(&mut self) -> Option<&mut SessionMemory> {
        self.session_memory.as_mut()
    }

    pub fn apply_session_memory_context(&self, messages: &[Message]) -> Vec<Message> {
        let Some(session_context) = self
            .session_memory
            .as_ref()
            .map(SessionMemory::render_as_system_context)
            .filter(|context| !context.is_empty())
        else {
            return messages.to_vec();
        };
        let mut updated = messages.to_vec();
        if let Some(system_message) = updated
            .iter_mut()
            .find(|message| message.role == MessageRole::System)
        {
            if !system_message.content.contains("<Session Memory>") {
                system_message.content.push_str("\n\n");
                system_message.content.push_str(&session_context);
            }
            return updated;
        }
        let mut system_message = Message::system(session_context);
        system_message.name = Some("session_memory".to_string());
        updated.insert(0, system_message);
        updated
    }

    pub fn strip_session_memory_context(&self, messages: &[Message]) -> Vec<Message> {
        let mut updated = messages.to_vec();
        let Some(system_message) = updated
            .iter_mut()
            .find(|message| message.role == MessageRole::System)
        else {
            return updated;
        };
        let Some(marker_index) = system_message.content.find("<Session Memory>") else {
            return updated;
        };
        system_message.content = system_message.content[..marker_index]
            .trim_end()
            .to_string();
        updated
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
