use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::sync::Arc;

use object_store::memory::InMemory;
use serde_json::{json, Value};
use vv_agent::workspace::{
    LocalWorkspaceBackend, MemoryWorkspaceBackend, S3WorkspaceBackend, WorkspaceBackend,
};
use vv_agent::{build_default_registry, ToolCall, ToolContext, ToolResultStatus};

#[path = "workspace_tools/backends.rs"]
mod backends;
#[path = "workspace_tools/file_tools.rs"]
mod file_tools;
#[path = "workspace_tools/listing.rs"]
mod listing;
#[path = "workspace_tools/read_write.rs"]
mod read_write;
#[path = "workspace_tools/security_paths.rs"]
mod security_paths;
