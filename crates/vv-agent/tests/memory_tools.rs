use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use vv_agent::{
    build_default_registry,
    memory::{token_utils::compute_compaction_threshold, CLEARED_MARKER},
    MemoryManager, MemoryManagerConfig, Message, SessionMemory, SessionMemoryConfig,
    SessionMemoryEntry, ToolCall, ToolContext, ToolResultStatus,
};

#[path = "memory_tools/compaction.rs"]
mod compaction;
#[path = "memory_tools/compress_tool.rs"]
mod compress_tool;
#[path = "memory_tools/session_memory.rs"]
mod session_memory;
