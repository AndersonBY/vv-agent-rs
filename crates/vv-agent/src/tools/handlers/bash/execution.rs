use std::time::Duration;

use serde_json::{json, Value};

use crate::runtime::background_sessions::background_session_manager;
use crate::runtime::processes::{
    read_captured_output, remove_captured_output, start_captured_process_with_env, wait_for_child,
};
use crate::runtime::shell::prepare_shell_execution;
use crate::tools::base::ToolContext;
use crate::tools::common::{
    bool_arg, integer_arg, path_escapes_workspace_error, string_arg, tool_error_with_code,
    tool_result_with_metadata, workspace_relative_path_or_absolute,
};
use crate::types::{Metadata, ToolArguments, ToolDirective, ToolExecutionResult, ToolResultStatus};

use super::env::build_process_env;
use super::shell_defaults::read_shell_defaults;

pub(super) fn execute_bash_command(
    context: &mut ToolContext,
    arguments: &ToolArguments,
) -> ToolExecutionResult {
    let command = string_arg(arguments.get("command"), "").trim().to_string();
    if command.is_empty() {
        return tool_error_with_code("`command` is required", "command_required");
    }
    if let Some(snippet) = blocked_dangerous_snippet(&command) {
        return tool_error_with_code(
            format!("dangerous command blocked: {snippet}"),
            "dangerous_command",
        );
    }

    let exec_dir = string_arg(arguments.get("exec_dir"), ".");
    let cwd = match context.resolve_workspace_path(&exec_dir) {
        Ok(cwd) => cwd,
        Err(error) => return path_escapes_workspace_error(error),
    };
    if !cwd.is_dir() {
        return tool_error_with_code(
            format!("exec_dir not found: {exec_dir}"),
            "invalid_exec_dir",
        );
    }

    let timeout_seconds = match read_timeout_seconds(arguments) {
        Ok(timeout_seconds) => timeout_seconds,
        Err(error) => return tool_error_with_code(error, "invalid_timeout"),
    };
    let stdin_text = arguments
        .contains_key("stdin")
        .then(|| string_arg(arguments.get("stdin"), ""));
    let auto_confirm = bool_arg(arguments.get("auto_confirm"), false);
    let run_in_background = bool_arg(arguments.get("run_in_background"), false);
    let (shell, windows_shell_priority, bash_env) = match read_shell_defaults(&context.metadata) {
        Ok(defaults) => defaults,
        Err(error) => return tool_error_with_code(error, "invalid_shell_config"),
    };
    let configured_shell = shell.clone();
    let process_env = build_process_env(bash_env.as_ref());
    let prepared = match prepare_shell_execution(
        &command,
        auto_confirm,
        stdin_text.as_deref(),
        shell.as_deref(),
        windows_shell_priority.as_deref(),
    ) {
        Ok(prepared) => prepared,
        Err(error) => return tool_error_with_code(error, "invalid_shell_config"),
    };
    let started = start_captured_process_with_env(
        &prepared.command,
        &cwd,
        prepared.stdin.as_deref(),
        process_env.as_ref(),
    );
    let mut started = match started {
        Ok(started) => started,
        Err(error) => {
            let shell = configured_shell
                .as_deref()
                .or(prepared.shell.as_deref())
                .unwrap_or("shell");
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
            configured_shell.clone(),
        );
        let mut payload = json!({
            "status": "running",
            "session_id": session_id,
        });
        if let Some(shell) = configured_shell {
            payload["shell"] = Value::String(shell);
        }
        let metadata = selected_metadata(&payload, &["status", "session_id", "shell"]);
        return tool_result_with_metadata(
            ToolResultStatus::Running,
            payload,
            None,
            ToolDirective::Continue,
            metadata,
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
            if let Some(shell) = configured_shell {
                payload["shell"] = Value::String(shell);
            }
            let metadata = selected_metadata(&payload, &["cwd", "exit_code", "shell"]);
            if exit_code == 0 {
                tool_result_with_metadata(
                    ToolResultStatus::Success,
                    payload,
                    None,
                    ToolDirective::Continue,
                    metadata,
                )
            } else {
                payload["ok"] = Value::Bool(false);
                payload["error"] = Value::String(format!("command exited with code {exit_code}"));
                payload["error_code"] = Value::String("command_failed".to_string());
                tool_result_with_metadata(
                    ToolResultStatus::Error,
                    payload,
                    Some("command_failed"),
                    ToolDirective::Continue,
                    metadata,
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
                configured_shell.clone(),
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
            if let Some(shell) = configured_shell {
                payload["shell"] = Value::String(shell);
            }
            let metadata = selected_metadata(
                &payload,
                &[
                    "status",
                    "session_id",
                    "cwd",
                    "shell",
                    "transitioned_to_background",
                ],
            );
            tool_result_with_metadata(
                ToolResultStatus::Running,
                payload,
                None,
                ToolDirective::Continue,
                metadata,
            )
        }
        Err(error) => tool_error_with_code(error.to_string(), "command_failed"),
    }
}

fn selected_metadata(payload: &Value, keys: &[&str]) -> Metadata {
    let Some(object) = payload.as_object() else {
        return Metadata::new();
    };
    keys.iter()
        .filter_map(|key| {
            let value = object.get(*key)?;
            (!value.is_null()).then(|| ((*key).to_string(), value.clone()))
        })
        .collect()
}

fn blocked_dangerous_snippet(command: &str) -> Option<&'static str> {
    let lowered = command.to_ascii_lowercase();
    [
        "rm -rf /",
        "shutdown",
        "reboot",
        "mkfs",
        "dd if=/dev/zero of=/dev/",
    ]
    .into_iter()
    .find(|snippet| lowered.contains(snippet))
}

fn read_timeout_seconds(arguments: &ToolArguments) -> Result<u64, String> {
    match arguments.get("timeout") {
        Some(value) => match integer_arg(value) {
            Ok(timeout) => Ok(timeout.clamp(1, 600) as u64),
            Err(_) => Err("`timeout` must be an integer".to_string()),
        },
        None => Ok(300),
    }
}
