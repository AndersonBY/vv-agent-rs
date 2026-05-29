use super::ShellInvocation;

pub(super) fn resolve_posix_shell(shell: Option<&str>) -> ShellInvocation {
    let selected = shell
        .map(str::trim)
        .filter(|value| !value.is_empty() && normalize_shell_name(value) != "auto")
        .unwrap_or("bash");
    resolve_named_shell(selected, false)
}

fn resolve_named_shell(shell: &str, windows: bool) -> ShellInvocation {
    let normalized = normalize_shell_name(shell);
    if windows && matches!(normalized.as_str(), "cmd" | "cmd.exe") {
        return ShellInvocation {
            kind: "cmd".to_string(),
            name: shell.to_string(),
            prefix: vec![shell.to_string(), "/C".to_string()],
        };
    }
    if matches!(normalized.as_str(), "powershell" | "powershell.exe") {
        return ShellInvocation {
            kind: "powershell".to_string(),
            name: shell.to_string(),
            prefix: vec![
                shell.to_string(),
                "-NoLogo".to_string(),
                "-NoProfile".to_string(),
                "-Command".to_string(),
            ],
        };
    }
    if matches!(normalized.as_str(), "pwsh" | "pwsh.exe") {
        return ShellInvocation {
            kind: "pwsh".to_string(),
            name: shell.to_string(),
            prefix: vec![
                shell.to_string(),
                "-NoLogo".to_string(),
                "-NoProfile".to_string(),
                "-Command".to_string(),
            ],
        };
    }
    if matches!(normalized.as_str(), "cmd" | "cmd.exe") {
        return ShellInvocation {
            kind: "cmd".to_string(),
            name: shell.to_string(),
            prefix: vec![shell.to_string(), "/c".to_string()],
        };
    }
    ShellInvocation {
        kind: infer_shell_kind(shell).to_string(),
        name: shell.to_string(),
        prefix: vec![shell.to_string(), "-lc".to_string()],
    }
}

pub(super) fn invocation_from_executable(kind: &str, executable: String) -> ShellInvocation {
    match kind {
        "cmd" => ShellInvocation {
            kind: "cmd".to_string(),
            name: executable.clone(),
            prefix: vec![executable, "/c".to_string()],
        },
        "powershell" => ShellInvocation {
            kind: "powershell".to_string(),
            name: executable.clone(),
            prefix: vec![
                executable,
                "-NoLogo".to_string(),
                "-NoProfile".to_string(),
                "-Command".to_string(),
            ],
        },
        "pwsh" => ShellInvocation {
            kind: "pwsh".to_string(),
            name: executable.clone(),
            prefix: vec![
                executable,
                "-NoLogo".to_string(),
                "-NoProfile".to_string(),
                "-Command".to_string(),
            ],
        },
        _ => ShellInvocation {
            kind: "bash".to_string(),
            name: executable.clone(),
            prefix: vec![executable, "-lc".to_string()],
        },
    }
}

pub(super) fn normalize_shell_name(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('_', "-")
}

pub(super) fn infer_shell_kind(executable_name: &str) -> &'static str {
    let lowered = executable_name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(executable_name)
        .trim()
        .to_ascii_lowercase();
    if matches!(lowered.as_str(), "cmd" | "cmd.exe") {
        "cmd"
    } else if lowered.starts_with("pwsh") {
        "pwsh"
    } else if lowered.contains("powershell") {
        "powershell"
    } else {
        "bash"
    }
}
