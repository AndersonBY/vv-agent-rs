use super::{
    helpers::{adjust_start_for_tool_context, sanitize_empty_assistant_messages},
    MemoryManager,
};
use crate::types::{Message, MessageRole};

impl MemoryManager {
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
}
