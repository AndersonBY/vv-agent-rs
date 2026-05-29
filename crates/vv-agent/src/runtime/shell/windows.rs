mod discovery;
mod priority;
mod programs;
mod resolve;
#[cfg(test)]
mod tests;

use discovery::RealWindowsShellDiscovery;
use resolve::resolve_windows_shell_with_discovery;

use super::ShellInvocation;

pub(super) fn resolve_windows_shell(
    shell: Option<&str>,
    windows_shell_priority: Option<&[String]>,
) -> Result<ShellInvocation, String> {
    resolve_windows_shell_with_discovery(shell, windows_shell_priority, &RealWindowsShellDiscovery)
}
