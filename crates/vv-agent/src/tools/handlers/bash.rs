use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::background_sessions::background_session_manager;
use crate::processes::{
    read_captured_output, remove_captured_output, start_captured_process_with_env, wait_for_child,
};
use crate::runtime::shell::{normalize_windows_shell_priority, prepare_shell_execution};
use crate::tools::base::{ToolContext, ToolSpec};
use crate::tools::common::{
    coerce_python_text_arg, parse_integer_arg, path_escapes_workspace_error, tool_error_with_code,
    tool_result, workspace_relative_path_or_absolute,
};
use crate::types::{ToolArguments, ToolDirective, ToolExecutionResult, ToolResultStatus};

const WINDOWS_PYTHON_ENV_DEFAULTS: [(&str, &str); 2] =
    [("PYTHONUTF8", "1"), ("PYTHONIOENCODING", "utf-8")];

pub fn run_bash_command(
    context: &mut ToolContext,
    arguments: &ToolArguments,
) -> ToolExecutionResult {
    let spec = bash_tool();
    (spec.handler)(context, arguments)
}

pub(crate) fn bash_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "bash",
        "Run a shell command in the current workspace.",
        Arc::new(|context, arguments| {
            let command = coerce_python_text_arg(arguments.get("command"), "")
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
            let exec_dir = coerce_python_text_arg(arguments.get("exec_dir"), ".");
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
            let timeout_seconds = match arguments.get("timeout") {
                Some(value) => match parse_integer_arg(value) {
                    Ok(timeout) => timeout.clamp(1, 600) as u64,
                    Err(_) => {
                        return tool_error_with_code(
                            "`timeout` must be an integer",
                            "invalid_timeout",
                        );
                    }
                },
                None => 300,
            };
            let stdin_text = arguments
                .contains_key("stdin")
                .then(|| coerce_python_text_arg(arguments.get("stdin"), ""));
            let auto_confirm = arguments
                .get("auto_confirm")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let run_in_background = arguments
                .get("run_in_background")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let (shell, windows_shell_priority, bash_env) =
                match read_shell_defaults(&context.metadata) {
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
        }),
    );
    if let Some(schema) = super::super::schemas::schema_for("bash") {
        spec.schema = schema;
    }
    spec
}

type ShellDefaults = (
    Option<String>,
    Option<Vec<String>>,
    Option<BTreeMap<String, String>>,
);

fn read_shell_defaults(metadata: &BTreeMap<String, Value>) -> Result<ShellDefaults, String> {
    let shell = metadata.get("bash_shell").and_then(normalize_shell_value);
    let windows_shell_priority =
        normalize_windows_shell_priority(metadata.get("windows_shell_priority"))?;
    let bash_env = normalize_bash_env(metadata.get("bash_env"))?;
    Ok((shell, windows_shell_priority, bash_env))
}

fn normalize_shell_value(value: &Value) -> Option<String> {
    let value = value_to_string(value).trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn normalize_bash_env(raw: Option<&Value>) -> Result<Option<BTreeMap<String, String>>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let Some(object) = raw.as_object() else {
        return Err("`bash_env` must be an object mapping env names to values".to_string());
    };
    let mut normalized = BTreeMap::new();
    for (key, value) in object {
        let env_name = key.trim();
        if env_name.is_empty() {
            return Err("`bash_env` contains empty env variable name".to_string());
        }
        normalized.insert(env_name.to_string(), value_to_string(value));
    }
    Ok(Some(normalized))
}

fn value_to_string(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(value) => value.clone(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        other => other.to_string(),
    }
}

fn build_process_env(
    extra_env: Option<&BTreeMap<String, String>>,
) -> Option<BTreeMap<String, String>> {
    build_process_env_for_platform(extra_env, cfg!(target_os = "windows"))
}

fn build_process_env_for_platform(
    extra_env: Option<&BTreeMap<String, String>>,
    windows: bool,
) -> Option<BTreeMap<String, String>> {
    build_process_env_with_base(extra_env, windows, std::env::vars().collect())
}

fn build_process_env_with_base(
    extra_env: Option<&BTreeMap<String, String>>,
    windows: bool,
    mut base_env: BTreeMap<String, String>,
) -> Option<BTreeMap<String, String>> {
    if !windows && extra_env.is_none() {
        return None;
    }
    if windows {
        for (key, value) in WINDOWS_PYTHON_ENV_DEFAULTS {
            base_env
                .entry(key.to_string())
                .or_insert_with(|| value.to_string());
        }
    }
    if let Some(extra_env) = extra_env {
        base_env.extend(extra_env.clone());
    }
    Some(base_env)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_env_injects_windows_python_encoding_defaults_like_python() {
        let process_env =
            build_process_env_with_base(None, true, BTreeMap::new()).expect("windows env");

        assert_eq!(process_env["PYTHONUTF8"], "1");
        assert_eq!(process_env["PYTHONIOENCODING"], "utf-8");
    }

    #[test]
    fn process_env_preserves_explicit_windows_python_encoding_overrides_like_python() {
        let process_env = build_process_env_with_base(
            Some(&BTreeMap::from([(
                "PYTHONIOENCODING".to_string(),
                "utf-8:replace".to_string(),
            )])),
            true,
            BTreeMap::from([
                ("PYTHONUTF8".to_string(), "0".to_string()),
                ("PYTHONIOENCODING".to_string(), "gbk".to_string()),
            ]),
        )
        .expect("windows env");

        assert_eq!(process_env["PYTHONUTF8"], "0");
        assert_eq!(process_env["PYTHONIOENCODING"], "utf-8:replace");
    }
}
