use super::{prompts, MemoryManager};
use crate::types::{Message, MessageRole};

impl MemoryManager {
    pub(super) fn maybe_append_memory_warning(
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
}
