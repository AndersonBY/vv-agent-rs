use std::path::{Path, PathBuf};

use super::path::{expand_home_path, find_executable_in_path, looks_like_path};
use super::platform::{infer_shell_kind, invocation_from_executable, normalize_shell_name};
use super::ShellInvocation;

const WINDOWS_DEFAULT_SHELL_PRIORITY: [&str; 1] = ["cmd"];
const WINDOWS_GIT_BASH_RELATIVE_PATHS: [&[&str]; 2] = [
    &["Git", "bin", "bash.exe"],
    &["Git", "usr", "bin", "bash.exe"],
];
const WINDOWS_POWERSHELL_RELATIVE_PATH: [&str; 4] =
    ["System32", "WindowsPowerShell", "v1.0", "powershell.exe"];

pub(super) fn resolve_windows_shell(
    shell: Option<&str>,
    windows_shell_priority: Option<&[String]>,
) -> Result<ShellInvocation, String> {
    resolve_windows_shell_with_discovery(shell, windows_shell_priority, &RealWindowsShellDiscovery)
}

trait WindowsShellDiscovery {
    fn env_var(&self, key: &str) -> Option<String>;
    fn which(&self, program: &str) -> Option<String>;
    fn is_file(&self, path: &Path) -> bool;
}

struct RealWindowsShellDiscovery;

impl WindowsShellDiscovery for RealWindowsShellDiscovery {
    fn env_var(&self, key: &str) -> Option<String> {
        std::env::var(key)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }

    fn which(&self, program: &str) -> Option<String> {
        find_executable_in_path(program).map(|path| path.to_string_lossy().to_string())
    }

    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }
}

fn resolve_windows_shell_with_discovery(
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

fn normalize_windows_priority(raw: Option<&[String]>) -> Vec<String> {
    let Some(raw) = raw.filter(|items| !items.is_empty()) else {
        return WINDOWS_DEFAULT_SHELL_PRIORITY
            .iter()
            .map(|item| item.to_string())
            .collect();
    };
    let mut normalized = Vec::new();
    for item in raw {
        let value = normalize_shell_name(item);
        if value.is_empty() || normalized.iter().any(|seen| seen == &value) {
            continue;
        }
        normalized.push(value);
    }
    if normalized.is_empty() {
        WINDOWS_DEFAULT_SHELL_PRIORITY
            .iter()
            .map(|item| item.to_string())
            .collect()
    } else {
        normalized
    }
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

fn resolve_windows_powershell(discovery: &impl WindowsShellDiscovery) -> Option<String> {
    for executable in ["powershell.exe", "powershell"] {
        if let Some(resolved) = discovery.which(executable) {
            return Some(resolved);
        }
    }

    let system_root = discovery.env_var("SYSTEMROOT")?;
    let mut candidate = PathBuf::from(system_root);
    for component in WINDOWS_POWERSHELL_RELATIVE_PATH {
        candidate.push(component);
    }
    discovery
        .is_file(&candidate)
        .then(|| candidate.to_string_lossy().to_string())
}

fn resolve_windows_git_bash(discovery: &impl WindowsShellDiscovery) -> Option<String> {
    let mut candidates = Vec::<PathBuf>::new();
    for root in windows_program_roots(discovery) {
        for relative_path in WINDOWS_GIT_BASH_RELATIVE_PATHS {
            let mut candidate = PathBuf::from(&root);
            for component in relative_path {
                candidate.push(component);
            }
            if !candidates.iter().any(|seen| seen == &candidate) {
                candidates.push(candidate);
            }
        }
    }

    for candidate in candidates {
        if discovery.is_file(&candidate) {
            return Some(candidate.to_string_lossy().to_string());
        }
    }

    discovery
        .which("bash.exe")
        .or_else(|| discovery.which("bash"))
}

fn windows_program_roots(discovery: &impl WindowsShellDiscovery) -> Vec<String> {
    let mut roots = Vec::new();
    for key in [
        "ProgramW6432",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "LocalAppData",
    ] {
        let Some(root) = discovery.env_var(key) else {
            continue;
        };
        if !roots.iter().any(|seen| seen == &root) {
            roots.push(root);
        }
    }
    roots
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct FakeWindowsShellDiscovery {
        env: BTreeMap<String, String>,
        which: BTreeMap<String, String>,
        files: Vec<PathBuf>,
    }

    impl WindowsShellDiscovery for FakeWindowsShellDiscovery {
        fn env_var(&self, key: &str) -> Option<String> {
            self.env.get(key).cloned()
        }

        fn which(&self, program: &str) -> Option<String> {
            self.which.get(program).cloned()
        }

        fn is_file(&self, path: &Path) -> bool {
            self.files.iter().any(|item| item == path)
        }
    }

    #[test]
    fn windows_shell_priority_skips_unavailable_git_bash() {
        let priority = vec!["git-bash".to_string(), "cmd".to_string()];
        let discovery = FakeWindowsShellDiscovery::default();

        let resolved = resolve_windows_shell_with_discovery(None, Some(&priority), &discovery)
            .expect("resolved shell");

        assert_eq!(resolved.kind, "cmd");
        assert!(resolved
            .prefix
            .first()
            .expect("program")
            .to_ascii_lowercase()
            .ends_with("cmd.exe"));
        assert_eq!(resolved.prefix[1], "/c");
    }

    #[test]
    fn windows_shell_priority_prefers_available_git_bash() {
        let priority = vec![
            "git-bash".to_string(),
            "powershell".to_string(),
            "cmd".to_string(),
        ];
        let discovery = FakeWindowsShellDiscovery {
            env: BTreeMap::from([("ProgramFiles".to_string(), "C:\\Program Files".to_string())]),
            files: vec![PathBuf::from("C:\\Program Files/Git/bin/bash.exe")],
            ..FakeWindowsShellDiscovery::default()
        };

        let resolved = resolve_windows_shell_with_discovery(None, Some(&priority), &discovery)
            .expect("resolved shell");

        assert_eq!(resolved.kind, "bash");
        assert_eq!(
            resolved.prefix,
            vec!["C:\\Program Files/Git/bin/bash.exe", "-lc"]
        );
    }

    #[test]
    fn windows_shell_priority_falls_back_to_powershell() {
        let priority = vec![
            "git-bash".to_string(),
            "powershell".to_string(),
            "cmd".to_string(),
        ];
        let discovery = FakeWindowsShellDiscovery {
            which: BTreeMap::from([(
                "powershell.exe".to_string(),
                "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe".to_string(),
            )]),
            ..FakeWindowsShellDiscovery::default()
        };

        let resolved = resolve_windows_shell_with_discovery(None, Some(&priority), &discovery)
            .expect("resolved shell");

        assert_eq!(resolved.kind, "powershell");
        assert_eq!(
            resolved.prefix,
            vec![
                "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe",
                "-NoLogo",
                "-NoProfile",
                "-Command"
            ]
        );
    }

    #[test]
    fn windows_explicit_unavailable_shell_returns_error() {
        let discovery = FakeWindowsShellDiscovery::default();
        let error = resolve_windows_shell_with_discovery(Some("git-bash"), None, &discovery)
            .expect_err("explicit unavailable shell should error");

        assert!(error.contains("Configured shell is unavailable"));
    }
}
