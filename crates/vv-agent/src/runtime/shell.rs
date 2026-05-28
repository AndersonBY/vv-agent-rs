use std::path::{Path, PathBuf};

const WINDOWS_AUTO_CONFIRM_LINES: usize = 512;
const WINDOWS_DEFAULT_SHELL_PRIORITY: [&str; 1] = ["cmd"];
const WINDOWS_GIT_BASH_RELATIVE_PATHS: [&[&str]; 2] = [
    &["Git", "bin", "bash.exe"],
    &["Git", "usr", "bin", "bash.exe"],
];
const WINDOWS_POWERSHELL_RELATIVE_PATH: [&str; 4] =
    ["System32", "WindowsPowerShell", "v1.0", "powershell.exe"];

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
        if python_falsy(item) {
            continue;
        }
        let value = python_str(item);
        let value = value.trim();
        if value.is_empty() || normalized.iter().any(|seen| seen == value) {
            continue;
        }
        normalized.push(value.to_string());
    }
    Ok(Some(normalized))
}

fn python_falsy(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => true,
        serde_json::Value::Bool(value) => !*value,
        serde_json::Value::Number(number) => number.as_f64() == Some(0.0),
        serde_json::Value::String(value) => value.is_empty(),
        serde_json::Value::Array(items) => items.is_empty(),
        serde_json::Value::Object(object) => object.is_empty(),
    }
}

fn python_str(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => String::new(),
        serde_json::Value::Bool(true) => "True".to_string(),
        serde_json::Value::Bool(false) => "False".to_string(),
        serde_json::Value::Number(number) => number.to_string(),
        serde_json::Value::String(value) => value.clone(),
        serde_json::Value::Array(items) => {
            let items = items.iter().map(python_repr).collect::<Vec<_>>().join(", ");
            format!("[{items}]")
        }
        serde_json::Value::Object(object) => {
            let items = object
                .iter()
                .map(|(key, value)| format!("{}: {}", python_repr_string(key), python_repr(value)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{{items}}}")
        }
    }
}

fn python_repr(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(value) => python_repr_string(value),
        other => python_str(other),
    }
}

fn python_repr_string(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn resolve_windows_shell(
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

fn invocation_from_executable(kind: &str, executable: String) -> ShellInvocation {
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

fn looks_like_path(value: &str) -> bool {
    value.contains('/') || value.contains('\\') || value.contains(':')
}

fn expand_home_path(value: &str) -> PathBuf {
    if value == "~" {
        if let Some(home) = home_dir() {
            return home;
        }
    } else if let Some(rest) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(value)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn find_executable_in_path(program: &str) -> Option<PathBuf> {
    let path = Path::new(program);
    if looks_like_path(program) {
        return path.is_file().then(|| path.to_path_buf());
    }

    let path_var = std::env::var_os("PATH")?;
    let extensions = executable_extensions(program);
    for directory in std::env::split_paths(&path_var) {
        for extension in &extensions {
            let candidate = directory.join(format!("{program}{extension}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn executable_extensions(program: &str) -> Vec<String> {
    if !cfg!(target_os = "windows") || Path::new(program).extension().is_some() {
        return vec![String::new()];
    }
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    let mut extensions = vec![String::new()];
    for extension in pathext.split(';') {
        let extension = extension.trim();
        if extension.is_empty() {
            continue;
        }
        let normalized = if extension.starts_with('.') {
            extension.to_string()
        } else {
            format!(".{extension}")
        };
        if !extensions.iter().any(|seen| seen == &normalized) {
            extensions.push(normalized);
        }
    }
    extensions
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
    fn windows_shell_priority_skips_unavailable_git_bash_like_python() {
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
    fn windows_shell_priority_prefers_available_git_bash_like_python() {
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
    fn windows_shell_priority_falls_back_to_powershell_like_python() {
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
    fn windows_explicit_unavailable_shell_returns_error_like_python() {
        let discovery = FakeWindowsShellDiscovery::default();
        let error = resolve_windows_shell_with_discovery(Some("git-bash"), None, &discovery)
            .expect_err("explicit unavailable shell should error");

        assert!(error.contains("Configured shell is unavailable"));
    }
}
