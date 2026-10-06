mod config;
mod content;
pub use config::ToolResultArtifactConfig;
pub(crate) use content::{
    build_compacted_tool_content, has_recovery_envelope, is_compacted_tool_content,
};
pub const TOOL_RESULT_COMPACT_MARKER: &str = "<Tool Result Compact>";
