use serde_json::{json, Value};

const BASH_DESCRIPTION: &str = "Run a command in the configured workspace shell. Prefer specialized file tools for direct file work. Long commands may run in the background; oversized terminal output returns a head/tail preview and a workspace artifact path for complete recovery.";

const CHECK_BACKGROUND_COMMAND_DESCRIPTION: &str = "Read the current state and bounded output of a background command by session id. Terminal oversized output uses the same preview-and-artifact result as foreground bash.";

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
                    "timeout": {"type": "integer", "description": "Foreground timeout seconds; bounded by the schema."},
                    "stdin": {"type": "string", "description": "Optional stdin content for interactive prompts, confirmation text, heredoc-style input, or commands that read from standard input."},
                    "auto_confirm": {"type": "boolean", "description": "Pipe yes to the command for non-interactive confirmation prompts."},
                    "run_in_background": {"type": "boolean", "description": "Start asynchronously and return a session id."}
                },
                "required": ["command"]
            }
        }
    })
}

pub(super) fn check_background_command_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "check_background_command",
            "description": CHECK_BACKGROUND_COMMAND_DESCRIPTION,
            "parameters": {
                "type": "object",
                "properties": {"session_id": {"type": "string", "description": "Session id returned by bash."}},
                "required": ["session_id"]
            }
        }
    })
}
