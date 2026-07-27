#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResultArtifactConfig {
    pub artifact_namespace: String,
    pub compact_threshold: usize,
    pub keep_last: usize,
    pub excerpt_head: usize,
    pub excerpt_tail: usize,
}

impl Default for ToolResultArtifactConfig {
    fn default() -> Self {
        Self {
            artifact_namespace: "memory".to_string(),
            compact_threshold: 2_000,
            keep_last: 3,
            excerpt_head: 200,
            excerpt_tail: 200,
        }
    }
}
