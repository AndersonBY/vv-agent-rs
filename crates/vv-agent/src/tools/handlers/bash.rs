use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::background_sessions::background_session_manager;
use crate::processes::{
    read_captured_output, remove_captured_output, start_captured_process, wait_for_child,
};
use crate::tools::base::ToolSpec;
use crate::tools::common::{
    path_escapes_workspace_error, tool_error_with_code, tool_result,
    workspace_relative_path_or_absolute,
};
use crate::types::{ToolDirective, ToolExecutionResult, ToolResultStatus};

pub(crate) fn bash_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "bash",
        "Run a shell command in the current workspace.",
        Arc::new(|context, arguments| {
            let command = arguments
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            if command.is_empty() {
                return tool_error_with_code("`command` is required", "command_required");
            }
            let lowered = command.to_ascii_lowercase();
            for snippet in [
                "rm -rf /",
                "shutdown",
                "reboot",
                "mkfs",
                "dd if=/dev/zero of=/dev/",
            ] {
                if lowered.contains(snippet) {
                    return tool_error_with_code(
                        format!("dangerous command blocked: {snippet}"),
                        "dangerous_command",
                    );
                }
            }
            let exec_dir = arguments
                .get("exec_dir")
                .and_then(Value::as_str)
                .unwrap_or(".");
            let cwd = match context.resolve_workspace_path(exec_dir) {
                Ok(cwd) => cwd,
                Err(error) => return path_escapes_workspace_error(error),
            };
            if !cwd.is_dir() {
                return tool_error_with_code(
                    format!("exec_dir not found: {exec_dir}"),
                    "invalid_exec_dir",
                );
            }
            let timeout_seconds = arguments
                .get("timeout")
                .and_then(Value::as_u64)
                .unwrap_or(300)
                .clamp(1, 600);
            let stdin_text = arguments.get("stdin").and_then(Value::as_str);
            let auto_confirm = arguments
                .get("auto_confirm")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let run_in_background = arguments
                .get("run_in_background")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let shell = context.metadata.get("bash_shell").and_then(Value::as_str);
            let prepared = prepare_shell_execution(&command, auto_confirm, stdin_text, shell);
            let started =
                start_captured_process(&prepared.command, &cwd, prepared.stdin.as_deref());
            let mut started = match started {
                Ok(started) => started,
                Err(error) => {
                    let shell = prepared.shell.unwrap_or_else(|| "shell".to_string());
                    return tool_error_with_code(
                        format!("Failed to start {shell}: {error}"),
                        "command_failed",
                    );
                }
            };

            if run_in_background {
                let session_id = background_session_manager().adopt_running_process(
                    command,
                    cwd,
                    timeout_seconds,
                    started.child,
                    started.output_path,
                    prepared.shell,
                );
                let payload = json!({
                    "status": "running",
                    "session_id": session_id,
                });
                return tool_result(
                    ToolResultStatus::Running,
                    payload,
                    None,
                    ToolDirective::Continue,
                );
            }

            match wait_for_child(&mut started.child, Duration::from_secs(timeout_seconds)) {
                Ok(Some(exit_status)) => {
                    let output = read_captured_output(&started.output_path, 50_000);
                    remove_captured_output(&started.output_path);
                    let exit_code = exit_status.code().unwrap_or(-1);
                    let mut payload = json!({
                        "cwd": workspace_relative_path_or_absolute(&context.workspace, &cwd),
                        "exit_code": exit_code,
                        "output": output,
                    });
                    if let Some(shell) = prepared.shell {
                        payload["shell"] = Value::String(shell);
                    }
                    if exit_code == 0 {
                        ToolExecutionResult::success("", payload.to_string())
                    } else {
                        tool_result(
                            ToolResultStatus::Error,
                            payload,
                            Some("command_failed"),
                            ToolDirective::Continue,
                        )
                    }
                }
                Ok(None) => {
                    let output = read_captured_output(&started.output_path, 50_000);
                    let session_id = background_session_manager().adopt_running_process(
                        command,
                        cwd,
                        timeout_seconds,
                        started.child,
                        started.output_path,
                        prepared.shell.clone(),
                    );
                    let mut payload = json!({
                        "status": "running",
                        "session_id": session_id,
                        "cwd": exec_dir,
                        "message": format!(
                            "command exceeded foreground timeout after {timeout_seconds} seconds and continues in background; use `check_background_command` with this session_id to inspect progress"
                        ),
                        "output": output,
                        "transitioned_to_background": true,
                    });
                    if let Some(shell) = prepared.shell {
                        payload["shell"] = Value::String(shell);
                    }
                    tool_result(
                        ToolResultStatus::Running,
                        payload,
                        None,
                        ToolDirective::Continue,
                    )
                }
                Err(error) => tool_error_with_code(error.to_string(), "command_failed"),
            }
        }),
    );
    if let Some(schema) = super::super::schemas::schema_for("bash") {
        spec.schema = schema;
    }
    spec
}

struct PreparedShellCommand {
    command: Vec<String>,
    shell: Option<String>,
    stdin: Option<String>,
}

struct ResolvedShellInvocation {
    kind: String,
    name: String,
    prefix: Vec<String>,
}

fn prepare_shell_execution(
    command: &str,
    auto_confirm: bool,
    stdin: Option<&str>,
    shell: Option<&str>,
) -> PreparedShellCommand {
    let resolved = resolve_shell_invocation(shell);
    if !auto_confirm {
        let mut prepared = resolved.prefix.clone();
        prepared.push(command.to_string());
        return PreparedShellCommand {
            command: prepared,
            shell: Some(resolved.name),
            stdin: stdin.map(str::to_string),
        };
    }

    if resolved.kind == "bash" {
        let mut prepared = resolved.prefix.clone();
        prepared.push(format!("yes | ({command})"));
        PreparedShellCommand {
            command: prepared,
            shell: Some(resolved.name),
            stdin: stdin.map(str::to_string),
        }
    } else {
        let mut prepared = resolved.prefix.clone();
        prepared.push(command.to_string());
        PreparedShellCommand {
            command: prepared,
            shell: Some(resolved.name),
            stdin: Some(format!("{}{}", "y\n".repeat(512), stdin.unwrap_or(""))),
        }
    }
}

fn resolve_shell_invocation(shell: Option<&str>) -> ResolvedShellInvocation {
    if cfg!(target_os = "windows") {
        let selected = shell
            .map(str::trim)
            .filter(|value| !value.is_empty() && normalize_shell_name(value) != "auto")
            .unwrap_or("cmd");
        return resolve_named_shell(selected, true);
    }

    let selected = shell
        .map(str::trim)
        .filter(|value| !value.is_empty() && normalize_shell_name(value) != "auto")
        .unwrap_or("bash");
    resolve_named_shell(selected, false)
}

fn resolve_named_shell(shell: &str, windows: bool) -> ResolvedShellInvocation {
    let normalized = normalize_shell_name(shell);
    if windows && matches!(normalized.as_str(), "cmd" | "cmd.exe") {
        return ResolvedShellInvocation {
            kind: "cmd".to_string(),
            name: shell.to_string(),
            prefix: vec![shell.to_string(), "/C".to_string()],
        };
    }
    if matches!(normalized.as_str(), "powershell" | "powershell.exe") {
        return ResolvedShellInvocation {
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
        return ResolvedShellInvocation {
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
        return ResolvedShellInvocation {
            kind: "cmd".to_string(),
            name: shell.to_string(),
            prefix: vec![shell.to_string(), "/c".to_string()],
        };
    }
    let kind = infer_shell_kind(shell);
    ResolvedShellInvocation {
        kind: kind.to_string(),
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
