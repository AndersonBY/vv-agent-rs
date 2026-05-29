use serde_json::{json, Value};

pub(super) fn create_sub_task_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "create_sub_task",
            "description": "Create sub-tasks for a configured sub-agent.\n\nModes:\n- Single task: provide `task_description` (+ optional `output_requirements`)\n- Batch task: provide `tasks` array for multiple independent tasks of the same sub-agent. Use this for parallel work that can be split into independent investigations, file reads, reviews, or implementation checks.\n\nExecution:\n- `wait_for_completion=true` (default): wait for result(s) and return final payload. Batch mode may run requests through the runtime execution backend in parallel and returns a summary plus one result per task.\n- `wait_for_completion=false`: start background sub-task(s) and return `task_id` / `task_ids` for later polling.\n- Batch payloads can include partial failures; inspect the summary and each result before deciding whether the parent task can continue.\n\nUse `sub_task_status` later to inspect progress, fetch results, or send follow-up messages.",
            "parameters": {
                "type": "object",
                "properties": {
                    "agent_id": {"type": "string", "description": "Exact sub-agent identifier from the configured `sub_agents` mapping. Do not pass a display name, model name, or inferred label."},
                    "task_description": {"type": "string", "description": "Single-task description for one self-contained objective. Mutually exclusive with `tasks`; give a concrete objective, constraints, and the evidence or deliverable expected by the parent Agent."},
                    "output_requirements": {"type": "string", "description": "Optional output constraints for single-task mode. State success criteria, expected format, and concrete deliverables the parent Agent needs."},
                    "tasks": {
                        "type": "array",
                        "description": "Batch mode: multiple independent tasks for the same sub-agent. Use when parallel work can be safely delegated without shared mutable state.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "task_description": {"type": "string", "description": "Task description for one independent sub-task. Give a concrete objective, relevant constraints, and the evidence or deliverable expected by the parent Agent."},
                                "output_requirements": {"type": "string", "description": "Optional output constraints for one sub-task. State success criteria, expected format, and concrete deliverables."}
                            },
                            "required": ["task_description"]
                        }
                    },
                    "include_main_summary": {"type": "boolean", "description": "Whether to include parent-task summary context. Default false. Use when the child needs parent context; keep false for independent tasks."},
                    "exclude_files_pattern": {"type": "string", "description": "Optional regex for excluding files from shared context. Use to keep large, generated, or irrelevant paths out of child context."},
                    "wait_for_completion": {"type": "boolean", "description": "Whether to wait for completion. Default true; false starts background execution. When false, returned task ids can be polled with `sub_task_status`."}
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
            "description": "Inspect sub-task status and optionally interact with a sub-task.\n\nCapabilities:\n- Query one or more sub-task ids\n- Return lightweight snapshot progress (`detail_level=snapshot`)\n- Send `message` to the first task id to steer a running task or continue a completed one\n- Optionally wait for the follow-up response with `wait_for_response=true`\n\nInteraction rules:\n- When `message` is provided, only the first task id is targeted.\n- Running tasks receive the message as queued steering input.\n- Completed tasks are continued in the same sub-agent session unless they stopped at `max_cycles`.\n- Use snapshot detail when deciding whether to wait, send a follow-up, or summarize child work.",
            "parameters": {
                "type": "object",
                "properties": {
                    "task_ids": {"type": "array", "description": "Sub-task ids to query. Use the ids returned by `create_sub_task`; duplicate ids are deduplicated. When `message` is provided, only the first id is used as the target.", "items": {"type": "string"}},
                    "message": {"type": "string", "description": "Optional follow-up or steering message for the first task id. Can steer a running task or continue a completed one."},
                    "detail_level": {"type": "string", "enum": ["basic", "snapshot"], "description": "Status response detail level. `snapshot` includes recent activity, latest tool call, and workspace files."},
                    "workspace_file_limit": {"type": "integer", "minimum": 1, "maximum": 100, "description": "Maximum number of workspace files returned per task in snapshot mode. Default 20. Lower this when file lists add noise; raise it only when files are needed to assess progress."},
                    "wait_for_response": {"type": "boolean", "description": "When `message` is provided, wait until the task finishes processing that message. Use true after sending `message` when the parent Agent needs the follow-up result before continuing; keep false for lightweight steering of a still-running child."}
                },
                "required": ["task_ids"]
            }
        }
    })
}
