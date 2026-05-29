mod command;
mod metadata;
mod path;
mod platform;
mod windows;

pub use command::{build_shell_invocation, prepare_shell_execution};
pub use metadata::normalize_windows_shell_priority;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellInvocation {
    pub kind: String,
    pub name: String,
    pub prefix: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedShellCommand {
    pub kind: String,
    pub command: Vec<String>,
    pub shell: Option<String>,
    pub stdin: Option<String>,
}

pub fn resolve_shell_invocation(
    shell: Option<&str>,
    windows_shell_priority: Option<&[String]>,
) -> Result<ShellInvocation, String> {
    if cfg!(target_os = "windows") {
        return windows::resolve_windows_shell(shell, windows_shell_priority);
    }
    Ok(platform::resolve_posix_shell(shell))
}
