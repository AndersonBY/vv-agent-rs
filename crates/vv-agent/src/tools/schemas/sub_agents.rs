use serde_json::{json, Value};

pub(super) fn create_sub_task_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "create_sub_task",
            "description": "Create sub-tasks for a configured sub-agent.\n\nModes:\n- Single task: provide `task_description` (+ optional `output_requirements`)\n- Batch task: provide `tasks` array for multiple independent tasks of the same sub-agent\n\nExecution:\n- `wait_for_completion=true` (default): wait for result(s) and return final payload\n- `wait_for_completion=false`: start background sub-task(s) and return `task_id` / `task_ids`\n\nUse `sub_task_status` later to inspect progress, fetch results, or send follow-up messages.",
            "parameters": {
                "type": "object",
                "properties": {
                    "agent_id": {"type": "string", "description": "Sub-agent identifier from configured sub_agents mapping."},
                    "task_description": {"type": "string", "description": "Single-task description. Mutually exclusive with `tasks`."},
                    "output_requirements": {"type": "string", "description": "Optional output constraints for single-task mode."},
                    "tasks": {
                        "type": "array",
                        "description": "Batch mode: multiple independent tasks for the same sub-agent.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "task_description": {"type": "string", "description": "Task description for one sub-task."},
                                "output_requirements": {"type": "string", "description": "Optional output constraints for one sub-task."}
                            },
                            "required": ["task_description"]
                        }
                    },
                    "include_main_summary": {"type": "boolean", "description": "Whether to include parent-task summary context. Default false."},
                    "exclude_files_pattern": {"type": "string", "description": "Optional regex for excluding files in shared context (reserved for compatibility)."},
                    "wait_for_completion": {"type": "boolean", "description": "Whether to wait for completion. Default true; false starts background execution."}
                },
                "required": ["agent_id"]
            }
        }
    })
}

pub(super) fn sub_task_status_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "sub_task_status",
            "description": "Inspect sub-task status and optionally interact with a sub-task.\n\nCapabilities:\n- Query one or more sub-task ids\n- Return lightweight snapshot progress (`detail_level=snapshot`)\n- Send `message` to the first task id to steer a running task or continue a completed one\n- Optionally wait for the follow-up response with `wait_for_response=true`",
            "parameters": {
                "type": "object",
                "properties": {
                    "task_ids": {"type": "array", "description": "Sub-task ids to query. When `message` is provided, only the first id is used as the target.", "items": {"type": "string"}},
                    "message": {"type": "string", "description": "Optional follow-up or steering message for the first task id."},
                    "detail_level": {"type": "string", "enum": ["basic", "snapshot"], "description": "Status response detail level. `snapshot` includes recent activity, latest tool call, and workspace files."},
                    "workspace_file_limit": {"type": "integer", "minimum": 1, "maximum": 100, "description": "Maximum number of workspace files returned per task in snapshot mode. Default 20."},
                    "wait_for_response": {"type": "boolean", "description": "When `message` is provided, wait until the task finishes processing that message."}
                },
                "required": ["task_ids"]
            }
        }
    })
}
