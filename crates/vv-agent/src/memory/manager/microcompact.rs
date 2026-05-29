use super::MemoryManager;
use crate::memory::microcompact::{microcompact, MicrocompactConfig};
use crate::types::Message;

impl MemoryManager {
    pub fn should_preemptive_microcompact(&self, message_length: u64) -> bool {
        let threshold = self.microcompact_trigger_threshold();
        threshold > 0 && message_length > threshold
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
}
