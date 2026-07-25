use serde_json::{json, Value};

const SUB_TASK_STATUS_DESCRIPTION: &str = "Inspect configured sub-task ids, optionally wait, or send one follow-up message to the first id. Snapshot mode adds recent activity and bounded workspace-file evidence.";

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
                        "minItems": 1,
                        "description": "Sub-task ids to query.",
                        "items": {"type": "string"}
                    },
                    "message": {
                        "type": "string",
                        "description": "Optional follow-up or steering message for the first task id."
                    },
                    "detail_level": {
                        "type": "string",
                        "enum": ["basic", "snapshot"],
                        "description": "Status response detail level."
                    },
                    "workspace_file_limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 100,
                        "description": "Maximum number of workspace files returned per task in snapshot mode."
                    },
                    "wait_for_completion": {
                        "type": "boolean",
                        "description": "Optional.",
                        "default": false
                    },
                    "check_interval_seconds": {
                        "type": "integer",
                        "minimum": 30,
                        "maximum": 1800,
                        "description": "Optional.",
                        "default": 300
                    },
                    "max_wait_seconds": {
                        "type": ["integer", "null"],
                        "minimum": 60,
                        "maximum": 86400,
                        "description": "Optional.",
                        "default": null
                    },
                    "wait_for_response": {
                        "type": "boolean",
                        "description": "When `message` is provided, wait until the task finishes processing that message."
                    }
                },
                "required": ["task_ids"]
            }
        }
    })
}
