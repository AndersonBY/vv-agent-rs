use serde_json::{json, Value};

pub(super) fn todo_write_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "todo_write",
            "description": "Create and manage structured TODO list for multi-step execution.\n\nProtocol:\n- Send the complete `todos` array each time.\n- Existing items with matching `id` are updated.\n- Items omitted from the new array are removed.\n- Only one item may have `status=in_progress`.\n\nUse this tool to keep task planning explicit and machine-readable.",
            "parameters": {
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "description": "Complete TODO list payload.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": {"type": "string", "description": "Existing TODO id for update; omit for new item."},
                                "title": {"type": "string", "description": "TODO title."},
                                "status": {"type": "string", "enum": ["pending", "in_progress", "completed"], "description": "TODO status."},
                                "priority": {"type": "string", "enum": ["low", "medium", "high"], "description": "TODO priority."}
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
