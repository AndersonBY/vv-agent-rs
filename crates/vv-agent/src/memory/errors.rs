use std::error::Error;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionExhaustedError {
    pub attempts: u32,
    pub last_error: Option<String>,
}

impl CompactionExhaustedError {
    pub fn new(attempts: u32, last_error: impl Into<Option<String>>) -> Self {
        Self {
            attempts,
            last_error: last_error.into(),
        }
    }
}

impl fmt::Display for CompactionExhaustedError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Context compaction failed after {} consecutive attempts. Last error: {}",
            self.attempts,
            self.last_error.as_deref().unwrap_or("None")
        )
    }
}

impl Error for CompactionExhaustedError {}
