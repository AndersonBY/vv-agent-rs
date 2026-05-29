use super::MemoryManager;
use crate::memory::session::SessionMemory;
use crate::types::{Message, MessageRole};

impl MemoryManager {
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
}
