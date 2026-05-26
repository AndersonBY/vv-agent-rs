use serde_json::{json, Value};

pub(super) fn bash_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "bash",
            "description": "Execute a shell command in the workspace using the runtime-selected shell.\n\nShell selection:\n- By default commands run through a POSIX-style shell (`bash -lc` on Unix-like hosts, `cmd /C` on Windows).\n- runtime metadata can override this with `bash_shell` (and Windows shell priority where available).\n- Returned payloads include the selected shell name so later polling/debugging can match the actual execution environment.\n\nGuidelines:\n- Prefer specialized read/write/search/edit tools when possible.\n- Use this tool for command execution, package install, scripts, and piped workflows.\n- For commands that may prompt for confirmation, pass `auto_confirm=true` or provide explicit `stdin`.\n- Use `run_in_background=true` for long-running commands and poll with check tool.\n- If a foreground command hits its timeout, it is automatically moved to a background\n  session and returns a `session_id` for polling.",
            "parameters": {
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "Shell command string executed through the configured shell."},
                    "exec_dir": {"type": "string", "description": "Execution directory (workspace-relative by default; absolute path allowed when outside-workspace access is enabled)."},
                    "timeout": {"type": "integer", "description": "Timeout seconds, default 300, max 600."},
                    "stdin": {"type": "string", "description": "Optional stdin content."},
                    "auto_confirm": {"type": "boolean", "description": "Pipe yes to command when true."},
                    "run_in_background": {"type": "boolean", "description": "Run command in background and return session_id for polling."}
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
            "description": "Check status/output for a command launched in background mode, including sessions auto-detached after foreground timeout.\n\nResponses can be `running`, `completed`, or an error with `background_command_failed`. Running responses include recent captured output when available; completed responses include final exit information and output. Poll this after `bash` returns a `session_id`, and stop polling once a terminal status is returned.",
            "parameters": {
                "type": "object",
                "properties": {"session_id": {"type": "string", "description": "Background session identifier returned by `bash` when `run_in_background=true` or when a foreground command times out."}},
                "required": ["session_id"]
            }
        }
    })
}
