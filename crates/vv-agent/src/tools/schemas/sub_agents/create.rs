use serde_json::{json, Value};

const CREATE_SUB_TASK_DESCRIPTION: &str = r#"Create sub-tasks for a configured sub-agent.

Modes:
- Single task: provide `task_description` (+ optional `output_requirements`) for one self-contained objective.
- Batch task: provide `tasks` array for multiple independent tasks of the same sub-agent. Use this for parallel work that can be split into independent investigations, file reads, reviews, or implementation checks.

Delegation rules:
- Use the exact sub-agent id from the configured `sub_agents` mapping.
- Give the child concrete scope, relevant files or commands, constraints, and expected evidence.
- Do not use batch mode for ordered edits, shared mutable state, dependent tasks, or work where one child result changes what the next child should do.
- Keep `include_main_summary=false` for independent tasks; enable it only when the child truly needs parent context.
- Use `exclude_files_pattern` to keep large, generated, or irrelevant paths out of child context.

Execution:
- `wait_for_completion=true` (default): wait for result(s) and return final payload. Batch mode may run requests through the runtime execution backend in parallel and returns a summary plus one result per task.
- `wait_for_completion=false`: start background sub-task(s) and return `task_id` / `task_ids` for later polling.
- Batch payloads can include partial failures; inspect the summary and each result before deciding whether the parent task can continue.

Result handling:
- For synchronous runs, read every returned result and error entry before using the child output.
- Treat partial failures as unresolved work unless the failed child was optional or its failure is itself the required evidence.
- For background runs, preserve the returned task ids and use `sub_task_status` later to inspect progress, fetch results, or send follow-up messages."#;

pub(in crate::tools::schemas) fn create_sub_task_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "create_sub_task",
            "description": CREATE_SUB_TASK_DESCRIPTION,
            "parameters": {
                "type": "object",
                "properties": {
                    "agent_id": {
                        "type": "string",
                        "description": "Exact sub-agent identifier from the configured `sub_agents` mapping. Do not pass a display name, model name, or inferred label."
                    },
                    "task_description": {
                        "type": "string",
                        "description": "Single-task description for one self-contained objective. Mutually exclusive with `tasks`; give a concrete objective, constraints, relevant files or commands, and the evidence or deliverable expected by the parent Agent."
                    },
                    "output_requirements": {
                        "type": "string",
                        "description": "Optional output constraints for single-task mode. State success criteria, expected format, concrete deliverables, and verification evidence the parent Agent needs."
                    },
                    "tasks": {
                        "type": "array",
                        "description": "Batch mode: multiple independent tasks for the same sub-agent. Use when parallel work can be safely delegated without shared mutable state or ordering dependencies.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "task_description": {
                                    "type": "string",
                                    "description": "Task description for one independent sub-task. Give a concrete objective, relevant constraints, files or commands, and the evidence or deliverable expected by the parent Agent."
                                },
                                "output_requirements": {
                                    "type": "string",
                                    "description": "Optional output constraints for one sub-task. State success criteria, expected format, concrete deliverables, and verification evidence."
                                }
                            },
                            "required": ["task_description"]
                        }
                    },
                    "include_main_summary": {
                        "type": "boolean",
                        "description": "Whether to include parent-task summary context. Default false. Use when the child needs parent context; keep false for independent tasks."
                    },
                    "exclude_files_pattern": {
                        "type": "string",
                        "description": "Optional regex for excluding files from shared context. Use to keep large, generated, or irrelevant paths out of child context."
                    },
                    "wait_for_completion": {
                        "type": "boolean",
                        "description": "Whether to wait for completion. Default true; false starts background execution. When false, returned task ids can be polled with `sub_task_status`."
                    }
                },
                "required": ["agent_id"]
            }
        }
    })
}
