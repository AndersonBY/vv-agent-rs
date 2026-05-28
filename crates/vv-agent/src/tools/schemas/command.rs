use serde_json::{json, Value};

const BASH_DESCRIPTION: &str = r#"Execute bash command in workspace.

Shell selection:
- By default commands run through a POSIX-style shell (`bash -lc` on Unix-like hosts, `cmd /C` on Windows).
- runtime metadata can override this with `bash_shell` (and Windows shell priority where available).
- runtime metadata `bash_env` can provide extra environment variables for foreground and background commands.
- Returned payloads include the selected shell name so later polling/debugging can match the actual execution environment.

Guidelines:
- Prefer specialized read/write/search/edit tools when possible.
- Use this tool for command execution, package install, scripts, and piped workflows.
- For commands that may prompt for confirmation, pass `auto_confirm=true` or provide explicit `stdin`.
- Use `run_in_background=true` for long-running commands and poll with check tool.
- If a foreground command hits its timeout, it is automatically moved to a background
  session and returns a `session_id` for polling."#;

const CHECK_BACKGROUND_COMMAND_DESCRIPTION: &str = r#"Check status/output for command launched in background mode, including sessions auto-detached after foreground timeout.

Check status and output for a command launched in background mode.

When to use:
- After `bash` returns a `session_id` from `run_in_background=true`.
- After a foreground command times out and the runtime auto-detaches it into a background session.
- When a long build, test, server, release, or watcher needs progress checks without blocking the main Agent loop.

Polling protocol:
- Poll until the response is terminal: `completed` or an error such as `background_command_failed`.
- A `running` response can include recent captured stdout/stderr; use that output to decide whether to wait, stop asking for status, or report a blocker.
- Stop polling once a terminal status is returned; repeated polling after completion should not be used as a substitute for reading the final payload.

Returns:
- Current session status, recent output while running, and final exit/output metadata on completion.
- Structured errors for unknown sessions, failed commands, and runtime-managed background failures."#;

pub(super) fn bash_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "bash",
            "description": BASH_DESCRIPTION,
            "parameters": {
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "Bash command string. The runtime executes it through the configured shell."},
                    "exec_dir": {"type": "string", "description": "Execution directory (workspace-relative by default; absolute path allowed when outside-workspace access is enabled)."},
                    "timeout": {"type": "integer", "description": "Timeout seconds, default 300, max 600."},
                    "stdin": {"type": "string", "description": "Optional stdin content for interactive prompts, confirmation text, heredoc-style input, or commands that read from standard input. Prefer explicit stdin over embedding secrets or fragile echo pipelines in the command string."},
                    "auto_confirm": {"type": "boolean", "description": "Pipe yes to the command for non-interactive confirmation prompts. Use carefully: do not enable for destructive operations unless the user has already authorized the action and the command target is explicit."},
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
            "description": CHECK_BACKGROUND_COMMAND_DESCRIPTION,
            "parameters": {
                "type": "object",
                "properties": {"session_id": {"type": "string", "description": "Background session identifier. It is returned by `bash` when `run_in_background=true` or when a foreground command times out."}},
                "required": ["session_id"]
            }
        }
    })
}
