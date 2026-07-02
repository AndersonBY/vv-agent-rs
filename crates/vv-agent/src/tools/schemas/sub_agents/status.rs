use serde_json::{json, Value};

const SUB_TASK_STATUS_DESCRIPTION: &str = r#"Inspect sub-task status and optionally interact with a sub-task.

Capabilities:
- Query one or more sub-task ids.
- Return lightweight snapshot progress (`detail_level=snapshot`).
- Send `message` to the first task id to steer a running task or continue a completed one.
- Wait for long-running background sub-task completion without repeated polling (`wait_for_completion=true`).
- Optionally wait for the follow-up response with `wait_for_response=true`.

Waiting:
- Use `wait_for_completion=true` when the parent Agent has no useful work until the background sub-task result is available.
- The runtime waits inside this tool call and returns when queried task(s) finish or `max_wait_seconds` is reached.
- Use `check_interval_seconds` as the suggested future re-check interval if the wait reaches its limit.

Continuation rules:
- When `message` is provided, only the first task id is targeted.
- Running tasks receive the message as queued steering input.
- Completed tasks are continued in the same sub-agent session unless they stopped at `max_cycles`.
- Do not continue a child task stopped at `max_cycles`; create a new task with clearer scope or report the child as blocked.
- Use `wait_for_response=true` only when the parent Agent needs the follow-up result before continuing.

Snapshot use:
- Use `detail_level=snapshot` when deciding whether to wait, send a follow-up, or summarize child work.
- Keep `workspace_file_limit` low when file lists add noise; raise it only when files are needed to assess progress."#;

pub(in crate::tools::schemas) fn sub_task_status_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "sub_task_status",
            "description": SUB_TASK_STATUS_DESCRIPTION,
            "parameters": {
                "type": "object",
                "properties": {
                    "task_ids": {
                        "type": "array",
                        "description": "Sub-task ids to query. Use the ids returned by `create_sub_task`; duplicate ids are deduplicated. When `message` is provided, only the first id is used as the target.",
                        "items": {"type": "string"}
                    },
                    "message": {
                        "type": "string",
                        "description": "Optional follow-up or steering message for the first task id. Can steer a running task or continue a completed one."
                    },
                    "detail_level": {
                        "type": "string",
                        "enum": ["basic", "snapshot"],
                        "description": "Status response detail level. `snapshot` includes recent activity, latest tool call, and workspace files."
                    },
                    "workspace_file_limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 100,
                        "description": "Maximum number of workspace files returned per task in snapshot mode. Default 20. Lower this when file lists add noise; raise it only when files are needed to assess progress."
                    },
                    "wait_for_completion": {
                        "type": "boolean",
                        "description": "Optional. If true and any queried sub-task is still running, wait inside this tool call until the task finishes or max_wait_seconds is reached. Use this for long-running background sub-tasks when the parent Agent needs the result before continuing.",
                        "default": false
                    },
                    "check_interval_seconds": {
                        "type": "integer",
                        "minimum": 30,
                        "maximum": 1800,
                        "description": "Optional. Used with wait_for_completion=true. Suggested re-check interval in seconds if max_wait_seconds is reached while tasks are still running. Default 300.",
                        "default": 300
                    },
                    "max_wait_seconds": {
                        "type": ["integer", "null"],
                        "minimum": 60,
                        "maximum": 86400,
                        "description": "Optional. Used with wait_for_completion=true. The maximum total wait time before returning the current still-running status to the Agent. Null or omitted uses the system default.",
                        "default": null
                    },
                    "wait_for_response": {
                        "type": "boolean",
                        "description": "When `message` is provided, wait until the task finishes processing that message. Use true after sending `message` when the parent Agent needs the follow-up result before continuing; keep false for lightweight steering of a still-running child."
                    }
                },
                "required": ["task_ids"]
            }
        }
    })
}
