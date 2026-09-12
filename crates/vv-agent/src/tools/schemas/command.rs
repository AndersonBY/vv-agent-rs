use serde_json::{json, Value};

const BASH_DESCRIPTION: &str = "Run a command in the configured workspace shell. Return its output when it exits within yield_time_ms, otherwise return a session_id and current output while it keeps running. Use check_background_command to inspect or stop_background_command to stop it. Oversized output has a recoverable workspace artifact.";

const CHECK_BACKGROUND_COMMAND_DESCRIPTION: &str = "Read the current state and bounded output of a command owned by this task and workspace. This is an immediate snapshot. A missing local session does not establish that its process exited. Oversized output includes a recoverable workspace artifact.";

const STOP_BACKGROUND_COMMAND_DESCRIPTION: &str = "Stop the process tree of a command owned by this task and workspace. Returns bounded output and observed exit status after termination is confirmed; stopping or unknown means termination is not yet confirmed. A missing local session does not establish that its process exited.";

pub(super) fn bash_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "bash",
            "description": BASH_DESCRIPTION,
            "parameters": {
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "Command to execute through the configured shell."},
                    "exec_dir": {"type": "string", "description": "Execution directory (workspace-relative by default; absolute path allowed when outside-workspace access is enabled)."},
                    "yield_time_ms": {"type": "integer", "default": 1000, "minimum": 0, "maximum": 10000, "description": "Maximum initial wait in milliseconds. Zero returns a handle immediately. Reaching this wait does not stop the command."},
                    "timeout_seconds": {"type": "integer", "minimum": 1, "maximum": 86400, "description": "Optional execution limit in seconds from process start, including time after returning a handle. Omit for no execution deadline. Querying never extends it."},
                    "stdin": {"type": "string", "description": "Optional stdin content for interactive prompts, confirmation text, heredoc-style input, or commands that read from standard input."},
                    "auto_confirm": {"type": "boolean", "default": false, "description": "Pipe yes to the command for non-interactive confirmation prompts."}
                },
                "required": ["command"],
                "additionalProperties": false
            }
        }
    })
}

pub(super) fn check_background_command_schema() -> Value {
    management_schema(
        "check_background_command",
        CHECK_BACKGROUND_COMMAND_DESCRIPTION,
    )
}

pub(super) fn stop_background_command_schema() -> Value {
    management_schema(
        "stop_background_command",
        STOP_BACKGROUND_COMMAND_DESCRIPTION,
    )
}

fn management_schema(name: &str, description: &str) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": {
                "type": "object",
                "properties": {"session_id": {"type": "string", "description": "Session id returned by bash."}},
                "required": ["session_id"],
                "additionalProperties": false
            }
        }
    })
}
