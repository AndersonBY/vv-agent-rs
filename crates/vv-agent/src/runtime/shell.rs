const WINDOWS_AUTO_CONFIRM_LINES: usize = 512;

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
        return resolve_windows_shell(shell, windows_shell_priority);
    }
    Ok(resolve_posix_shell(shell))
}

pub fn build_shell_invocation(
    command: &str,
    shell: Option<&str>,
    windows_shell_priority: Option<&[String]>,
) -> Result<Vec<String>, String> {
    let resolved = resolve_shell_invocation(shell, windows_shell_priority)?;
    let mut invocation = resolved.prefix;
    invocation.push(command.to_string());
    Ok(invocation)
}

pub fn prepare_shell_execution(
    command: &str,
    auto_confirm: bool,
    stdin: Option<&str>,
    shell: Option<&str>,
    windows_shell_priority: Option<&[String]>,
) -> Result<PreparedShellCommand, String> {
    let resolved = resolve_shell_invocation(shell, windows_shell_priority)?;
    if !auto_confirm {
        let mut prepared = resolved.prefix.clone();
        prepared.push(command.to_string());
        return Ok(PreparedShellCommand {
            kind: resolved.kind,
            command: prepared,
            shell: Some(resolved.name),
            stdin: stdin.map(str::to_string),
        });
    }

    if resolved.kind == "bash" {
        let mut prepared = resolved.prefix.clone();
        prepared.push(format!("yes | ({command})"));
        Ok(PreparedShellCommand {
            kind: resolved.kind,
            command: prepared,
            shell: Some(resolved.name),
            stdin: stdin.map(str::to_string),
        })
    } else {
        let mut prepared = resolved.prefix.clone();
        prepared.push(command.to_string());
        Ok(PreparedShellCommand {
            kind: resolved.kind,
            command: prepared,
            shell: Some(resolved.name),
            stdin: Some(format!(
                "{}{}",
                "y\n".repeat(WINDOWS_AUTO_CONFIRM_LINES),
                stdin.unwrap_or("")
            )),
        })
    }
}

pub fn normalize_windows_shell_priority(
    raw: Option<&serde_json::Value>,
) -> Result<Option<Vec<String>>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let Some(items) = raw.as_array() else {
        return Err("`windows_shell_priority` must be a list of shell names".to_string());
    };
    let mut normalized = Vec::new();
    for item in items {
        let value = item.as_str().unwrap_or_default().trim();
        if value.is_empty() || normalized.iter().any(|seen| seen == value) {
            continue;
        }
        normalized.push(value.to_string());
    }
    Ok(Some(normalized))
}

fn resolve_windows_shell(
    shell: Option<&str>,
    windows_shell_priority: Option<&[String]>,
) -> Result<ShellInvocation, String> {
    let selected = shell
        .map(str::trim)
        .filter(|value| !value.is_empty() && normalize_shell_name(value) != "auto")
        .map(str::to_string)
        .or_else(|| {
            windows_shell_priority
                .and_then(|priority| priority.first())
                .cloned()
        })
        .unwrap_or_else(|| "cmd".to_string());
    Ok(resolve_named_shell(&selected, true))
}

fn resolve_posix_shell(shell: Option<&str>) -> ShellInvocation {
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

fn normalize_shell_name(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('_', "-")
}

fn infer_shell_kind(executable_name: &str) -> &'static str {
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
