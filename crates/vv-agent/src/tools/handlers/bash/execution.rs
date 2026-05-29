use std::time::Duration;

use serde_json::{json, Value};

use crate::runtime::background_sessions::background_session_manager;
use crate::runtime::processes::{
    read_captured_output, remove_captured_output, start_captured_process_with_env, wait_for_child,
};
use crate::runtime::shell::prepare_shell_execution;
use crate::tools::base::ToolContext;
use crate::tools::common::{
    coerce_truthy_arg, parse_integer_arg, path_escapes_workspace_error, stringify_tool_arg,
    tool_error_with_code, tool_result, workspace_relative_path_or_absolute,
};
use crate::types::{ToolArguments, ToolDirective, ToolExecutionResult, ToolResultStatus};

use super::env::build_process_env;
use super::shell_defaults::read_shell_defaults;

pub(super) fn execute_bash_command(
    context: &mut ToolContext,
    arguments: &ToolArguments,
) -> ToolExecutionResult {
    let command = stringify_tool_arg(arguments.get("command"), "")
        .trim()
        .to_string();
    if command.is_empty() {
        return tool_error_with_code("`command` is required", "command_required");
    }
    if let Some(snippet) = blocked_dangerous_snippet(&command) {
        return tool_error_with_code(
            format!("dangerous command blocked: {snippet}"),
            "dangerous_command",
        );
    }

    let exec_dir = stringify_tool_arg(arguments.get("exec_dir"), ".");
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
        .then(|| stringify_tool_arg(arguments.get("stdin"), ""));
    let auto_confirm = json_truthy_argument(arguments, "auto_confirm");
    let run_in_background = json_truthy_argument(arguments, "run_in_background");
    let (shell, windows_shell_priority, bash_env) = match read_shell_defaults(&context.metadata) {
        Ok(defaults) => defaults,
        Err(error) => return tool_error_with_code(error, "invalid_shell_config"),
    };
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
        Some(value) => match parse_integer_arg(value) {
            Ok(timeout) => Ok(timeout.clamp(1, 600) as u64),
            Err(_) => Err("`timeout` must be an integer".to_string()),
        },
        None => Ok(300),
    }
}

fn json_truthy_argument(arguments: &ToolArguments, name: &str) -> bool {
    arguments
        .get(name)
        .map(|value| coerce_truthy_arg(Some(value), false))
        .unwrap_or(false)
}
