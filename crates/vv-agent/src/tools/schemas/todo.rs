use serde_json::{json, Value};

const TODO_WRITE_DESCRIPTION: &str = "Replace the current structured TODO list. Send the complete list, keep at most one item in_progress, and use stable ids when updating existing items.";

pub(super) fn todo_write_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "todo_write",
            "description": TODO_WRITE_DESCRIPTION,
            "parameters": {
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "description": "Complete TODO list replacement payload.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": {"type": "string", "description": "Existing TODO id for update; omit for new item. When omitted, a generated 8-character id is assigned."},
                                "title": {"type": "string", "description": "TODO title. Make it actionable and observable, so progress can be verified without reading hidden context."},
                                "status": {"type": "string", "enum": ["pending", "in_progress", "completed"], "description": "TODO status: `pending` for not started, `in_progress` for the single active item, or `completed` after verification."},
                                "priority": {"type": "string", "enum": ["low", "medium", "high"], "description": "TODO priority: `high` for blockers or user-critical work, `medium` for normal required work, `low` for cleanup or optional follow-up."}
                            },
                            "required": ["title", "status", "priority"]
                        }
                    }
                },
                "required": ["todos"]
            }
        }
    })
}
