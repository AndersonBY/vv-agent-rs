use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Child;
use std::time::Instant;

#[derive(Debug, Clone, Default)]
pub struct BackgroundSessionStartOptions {
    pub stdin: Option<String>,
    pub auto_confirm: bool,
    pub shell: Option<String>,
    pub windows_shell_priority: Option<Vec<String>>,
    pub env: Option<BTreeMap<String, String>>,
}

pub struct BackgroundSessionAdoptOptions {
    pub command: String,
    pub cwd: PathBuf,
    pub timeout_seconds: u64,
    pub child: Child,
    pub output_path: PathBuf,
    pub shell: Option<String>,
    pub started_at: Option<Instant>,
}

impl BackgroundSessionAdoptOptions {
    pub fn new(
        command: impl Into<String>,
        cwd: impl Into<PathBuf>,
        timeout_seconds: u64,
        child: Child,
        output_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            command: command.into(),
            cwd: cwd.into(),
            timeout_seconds,
            child,
            output_path: output_path.into(),
            shell: None,
            started_at: None,
        }
    }

    pub fn with_shell(mut self, shell: impl Into<String>) -> Self {
        self.shell = Some(shell.into());
        self
    }

    pub fn with_started_at(mut self, started_at: Instant) -> Self {
        self.started_at = Some(started_at);
        self
    }
}
