use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResultArtifactConfig {
    pub workspace: Option<PathBuf>,
    pub artifact_dir: PathBuf,
    pub compact_threshold: usize,
    pub keep_last: usize,
    pub excerpt_head: usize,
    pub excerpt_tail: usize,
}

impl Default for ToolResultArtifactConfig {
    fn default() -> Self {
        Self {
            workspace: None,
            artifact_dir: PathBuf::from(".memory/tool_results"),
            compact_threshold: 2_000,
            keep_last: 3,
            excerpt_head: 200,
            excerpt_tail: 200,
        }
    }
}
