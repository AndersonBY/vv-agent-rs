use serde_json::Value;

use crate::runtime::background_sessions::{
    background_session_manager, BackgroundSessionAdoptOptions,
};
use crate::runtime::processes::start_captured_process_with_env;
use crate::runtime::shell::prepare_shell_execution;
use crate::tools::base::ToolContext;
use crate::tools::common::{
    bool_arg, path_escapes_workspace_error, string_arg, tool_error_with_code,
    workspace_relative_path_or_absolute,
};
use crate::tools::handlers::background::background_command_result;
use crate::types::{ToolArguments, ToolExecutionResult};

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
    let yield_time_ms = match arguments
        .get("yield_time_ms")
        .map(|value| bounded_integer(value, "yield_time_ms", 0, 10000))
        .transpose()
    {
        Ok(value) => value.unwrap_or(1000),
        Err(error) => return tool_error_with_code(error, "invalid_tool_arguments"),
    };
    let timeout_seconds = match arguments
        .get("timeout_seconds")
        .map(|value| bounded_integer(value, "timeout_seconds", 1, 86400))
        .transpose()
    {
        Ok(value) => value,
        Err(error) => return tool_error_with_code(error, "invalid_tool_arguments"),
    };
    let stdin_text = arguments
        .contains_key("stdin")
        .then(|| string_arg(arguments.get("stdin"), ""));
    let (shell, priority, bash_env) = match read_shell_defaults(&context.metadata) {
        Ok(defaults) => defaults,
        Err(error) => return tool_error_with_code(error, "invalid_shell_config"),
    };
    let process_env = build_process_env(bash_env.as_ref());
    let prepared = match prepare_shell_execution(
        &command,
        bool_arg(arguments.get("auto_confirm"), false),
        stdin_text.as_deref(),
        shell.as_deref(),
        priority.as_deref(),
    ) {
        Ok(prepared) => prepared,
        Err(error) => return tool_error_with_code(error, "invalid_shell_config"),
    };
    let started = match start_captured_process_with_env(
        &prepared.command,
        &cwd,
        prepared.stdin.as_deref(),
        process_env.as_ref(),
    ) {
        Ok(started) => started,
        Err(error) => {
            return tool_error_with_code(
                format!(
                    "Failed to start {}: {error}",
                    shell
                        .as_deref()
                        .or(prepared.shell.as_deref())
                        .unwrap_or("shell")
                ),
                "command_failed",
            )
        }
    };
    let mut options = BackgroundSessionAdoptOptions::new(
        command,
        cwd.clone(),
        timeout_seconds,
        started.child,
        started.output_path,
    )
    .with_started_at(started.started_at)
    .with_owner(&context.task_id, context.workspace.clone())
    .with_artifact_context(
        context.effective_workspace_backend(),
        &context.task_id,
        &context.tool_call_id,
    );
    options.shell = shell;
    let manager = background_session_manager();
    let session_id = manager.adopt_running_process_with_options(options);
    manager.wait(&session_id, yield_time_ms);
    let payload = manager.check_for_tool(
        &session_id,
        context.effective_workspace_backend(),
        &context.task_id,
        &context.tool_call_id,
        &context.workspace,
    );
    background_command_result(
        payload,
        Some(workspace_relative_path_or_absolute(
            &context.workspace,
            &cwd,
        )),
        yield_time_ms == 0,
    )
}

fn bounded_integer(value: &Value, name: &str, minimum: u64, maximum: u64) -> Result<u64, String> {
    value
        .as_u64()
        .filter(|value| (minimum..=maximum).contains(value))
        .ok_or_else(|| format!("`{name}` must be an integer from {minimum} through {maximum}"))
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
