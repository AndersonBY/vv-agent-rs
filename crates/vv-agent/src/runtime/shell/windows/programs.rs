use std::path::PathBuf;

use super::discovery::WindowsShellDiscovery;

const WINDOWS_GIT_BASH_RELATIVE_PATHS: [&[&str]; 2] = [
    &["Git", "bin", "bash.exe"],
    &["Git", "usr", "bin", "bash.exe"],
];
const WINDOWS_POWERSHELL_RELATIVE_PATH: [&str; 4] =
    ["System32", "WindowsPowerShell", "v1.0", "powershell.exe"];

pub(super) fn resolve_windows_powershell(discovery: &impl WindowsShellDiscovery) -> Option<String> {
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

pub(super) fn resolve_windows_git_bash(discovery: &impl WindowsShellDiscovery) -> Option<String> {
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
