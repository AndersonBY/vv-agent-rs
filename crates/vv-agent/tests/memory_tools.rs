use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::json;
use vv_agent::{
    memory::{token_utils::compute_compaction_threshold, TOOL_RESULT_COMPACT_MARKER},
    MemoryManager, MemoryManagerConfig, Message, MicrocompactionPolicy, SessionMemory,
    SessionMemoryConfig, SessionMemoryEntry, ToolCall,
};

#[path = "memory_tools/compaction.rs"]
mod compaction;
#[path = "memory_tools/session_memory.rs"]
mod session_memory;
