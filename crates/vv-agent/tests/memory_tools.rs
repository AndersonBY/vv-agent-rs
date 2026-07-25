use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use serde_json::json;
use vv_agent::{
    memory::{token_utils::compute_compaction_threshold, CLEARED_MARKER},
    MemoryManager, MemoryManagerConfig, Message, SessionMemory, SessionMemoryConfig,
    SessionMemoryEntry, ToolCall,
};

#[path = "memory_tools/compaction.rs"]
mod compaction;
#[path = "memory_tools/session_memory.rs"]
mod session_memory;
