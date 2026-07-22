use serde_json::{json, Value};

const TODO_WRITE_DESCRIPTION: &str = r#"Create and manage structured TODO list for multi-step execution.

Protocol:
- Send the complete `todos` array each time.
- The payload is a replacement payload, not a patch.
- Existing items with matching `id` are updated.
- Matching items keep their original `created_at`.
- Items omitted from the new array are removed.
- Missing `id` values are generated automatically as short stable ids.
- Each item must include `title`, `status`, and `priority`.
- Only one item may have `status=in_progress`.

When to use:
- Track multi-step implementation, verification, review, release, or incident recovery work.
- Make progress state explicit before delegating, running long commands, or switching from investigation to edits.

Returns:
- The current TODO list with generated ids, timestamps, and validation errors when statuses conflict.

Use this tool to keep task planning explicit and machine-readable."#;

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
                        "description": "Complete TODO list replacement payload. Send every item that should remain; omitted existing ids are removed.",
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
