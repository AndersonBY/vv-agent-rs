use serde_json::{json, Value};

const TODO_WRITE_DESCRIPTION: &str = r#"Create and manage structured TODO list for multi-step execution.

Protocol:
- Send the complete `todos` array each time.
- The payload is a replacement payload, not a patch.
- Existing items with matching `id` are updated and keep their original `created_at`.
- Items omitted from the new array are removed.
- Missing `id` values are generated automatically as short stable ids.
- Missing status defaults to `pending`; missing priority defaults to `medium`.
- Only one item may have `status=in_progress`.

When to use:
- Track multi-step implementation, verification, migration, review, or incident recovery work.
- Make progress state explicit before delegating, running long commands, or switching from investigation to edits.

Returns:
- The normalized TODO list with generated ids/defaults and validation errors when statuses conflict.

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
                        "description": "Complete TODO list payload.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": {"type": "string", "description": "Existing TODO id for update; omit for a generated 8-character id."},
                                "title": {"type": "string", "description": "TODO title."},
                                "status": {"type": "string", "enum": ["pending", "in_progress", "completed"], "description": "TODO status. Defaults to pending when omitted."},
                                "priority": {"type": "string", "enum": ["low", "medium", "high"], "description": "TODO priority. Defaults to medium when omitted."}
                            },
                            "required": ["title"]
                        }
                    }
                },
                "required": ["todos"]
            }
        }
    })
}
