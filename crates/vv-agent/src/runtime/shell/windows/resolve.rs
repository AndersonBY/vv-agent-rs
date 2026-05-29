use std::path::Path;

use super::super::path::{expand_home_path, looks_like_path};
use super::super::platform::{infer_shell_kind, invocation_from_executable, normalize_shell_name};
use super::super::ShellInvocation;
use super::discovery::WindowsShellDiscovery;
use super::priority::normalize_windows_priority;
use super::programs::{resolve_windows_git_bash, resolve_windows_powershell};

pub(super) fn resolve_windows_shell_with_discovery(
    shell: Option<&str>,
    windows_shell_priority: Option<&[String]>,
    discovery: &impl WindowsShellDiscovery,
) -> Result<ShellInvocation, String> {
    let selected_shell = shell.unwrap_or_default().trim();
    if !selected_shell.is_empty() && normalize_shell_name(selected_shell) != "auto" {
        return resolve_windows_shell_entry(selected_shell, discovery).ok_or_else(|| {
            format!("Configured shell is unavailable on Windows: {selected_shell}")
        });
    }

    for entry in normalize_windows_priority(windows_shell_priority) {
        if let Some(invocation) = resolve_windows_shell_entry(&entry, discovery) {
            return Ok(invocation);
        }
    }

    resolve_windows_shell_entry("cmd", discovery)
        .ok_or_else(|| "Unable to resolve Windows command shell.".to_string())
}

fn resolve_windows_shell_entry(
    entry: &str,
    discovery: &impl WindowsShellDiscovery,
) -> Option<ShellInvocation> {
    let shell_name = normalize_shell_name(entry);
    if matches!(shell_name.as_str(), "cmd" | "cmd.exe") {
        let executable = discovery
            .env_var("COMSPEC")
            .or_else(|| discovery.which("cmd.exe"))
            .unwrap_or_else(|| "cmd.exe".to_string());
        return Some(invocation_from_executable("cmd", executable));
    }

    if matches!(shell_name.as_str(), "powershell" | "powershell.exe") {
        return resolve_windows_powershell(discovery)
            .map(|executable| invocation_from_executable("powershell", executable));
    }

    if matches!(shell_name.as_str(), "pwsh" | "pwsh.exe") {
        return discovery
            .which("pwsh.exe")
            .or_else(|| discovery.which("pwsh"))
            .map(|executable| invocation_from_executable("pwsh", executable));
    }

    if matches!(shell_name.as_str(), "git-bash" | "gitbash") {
        return resolve_windows_git_bash(discovery)
            .map(|executable| invocation_from_executable("bash", executable));
    }

    if matches!(shell_name.as_str(), "bash" | "bash.exe") {
        return discovery
            .which("bash.exe")
            .or_else(|| discovery.which("bash"))
            .or_else(|| resolve_windows_git_bash(discovery))
            .map(|executable| invocation_from_executable("bash", executable));
    }

    if shell_name.is_empty() {
        return None;
    }

    if looks_like_path(entry) {
        let candidate = expand_home_path(entry);
        if !discovery.is_file(&candidate) {
            return None;
        }
        let inferred = infer_shell_kind(
            candidate
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(entry),
        );
        return Some(invocation_from_executable(
            inferred,
            candidate.to_string_lossy().to_string(),
        ));
    }

    discovery.which(entry).map(|executable| {
        let inferred = infer_shell_kind(
            Path::new(&executable)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(&executable),
        );
        invocation_from_executable(inferred, executable)
    })
}
