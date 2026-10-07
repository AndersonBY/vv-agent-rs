#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResultArtifactConfig {
    pub excerpt_head: usize,
    pub excerpt_tail: usize,
}

impl Default for ToolResultArtifactConfig {
    fn default() -> Self {
        Self {
            excerpt_head: 200,
            excerpt_tail: 200,
        }
    }
}
