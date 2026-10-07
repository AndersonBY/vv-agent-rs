use super::MemoryManager;
use crate::memory::RuntimeMemoryCallbackError;
use crate::types::Message;
impl MemoryManager {
    pub fn emergency_compact(&mut self, messages: &[Message], drop_ratio: f64) -> Vec<Message> {
        self.emergency_compact_observed(messages, drop_ratio, None)
            .expect("public memory compaction has no runtime callback control flow")
    }
    pub(crate) fn emergency_compact_observed(
        &mut self,
        messages: &[Message],
        drop_ratio: f64,
        cycle: Option<u32>,
    ) -> Result<Vec<Message>, RuntimeMemoryCallbackError> {
        let keep = ((self.config.keep_recent_messages as f64 * (1.0 - drop_ratio.clamp(0.0, 0.95)))
            .floor() as usize)
            .max(1);
        let (candidate, changed) = self.summarize_prefix(
            messages,
            keep,
            cycle,
            self.runtime_callbacks.memory_compaction.as_ref(),
        )?;
        if changed {
            let tokens =
                crate::memory::token_utils::count_messages_tokens(&candidate, &self.config.model);
            if let Some(memory) = self.session_memory.as_mut() {
                memory.on_compaction(Some(tokens));
            }
        }
        Ok(candidate)
    }
}
