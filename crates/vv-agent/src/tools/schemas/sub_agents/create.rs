use serde_json::{json, Value};

const CREATE_SUB_TASK_DESCRIPTION: &str = "Run one task, or independent batch tasks, on a configured sub-agent. Use the exact agent id, keep tasks self-contained, and choose background execution only when the parent can continue independently.";

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
                        "description": "Exact sub-agent identifier from the configured `sub_agents` mapping."
                    },
                    "task_description": {
                        "type": "string",
                        "description": "Single-task description for one self-contained objective."
                    },
                    "output_requirements": {
                        "type": "string",
                        "description": "Optional output constraints for single-task mode."
                    },
                    "tasks": {
                        "type": "array",
                        "minItems": 1,
                        "description": "Batch mode: multiple independent tasks for the same sub-agent.",
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
                        "description": "Whether to include parent-task summary context."
                    },
                    "exclude_files_pattern": {
                        "type": "string",
                        "description": "Optional portable regex applied to normalized workspace-relative paths for child discovery only."
                    },
                    "wait_for_completion": {
                        "type": "boolean",
                        "description": "Whether to wait for completion."
                    }
                },
                "required": ["agent_id"]
            }
        }
    })
}
