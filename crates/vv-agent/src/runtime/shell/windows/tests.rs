use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::discovery::WindowsShellDiscovery;
use super::resolve::resolve_windows_shell_with_discovery;

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
