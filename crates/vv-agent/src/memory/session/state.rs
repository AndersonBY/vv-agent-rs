use serde::{Deserialize, Serialize};

use super::SessionMemoryEntry;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMemoryState {
    pub entries: Vec<SessionMemoryEntry>,
    pub last_extracted_message_index: i32,
    pub tokens_at_last_extraction: u64,
    pub initialized: bool,
}

impl Default for SessionMemoryState {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            last_extracted_message_index: -1,
            tokens_at_last_extraction: 0,
            initialized: false,
        }
    }
}
