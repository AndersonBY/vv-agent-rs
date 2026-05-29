use std::collections::BTreeSet;

use super::MemoryManager;
use crate::memory::token_utils::{compute_compaction_threshold, count_messages_tokens};
use crate::types::{Message, MessageRole};

impl MemoryManager {
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

    pub(super) fn calculate_effective_length(
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
}
