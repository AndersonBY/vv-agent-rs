use serde_json::{json, Value};

pub(super) fn read_image_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "read_image",
            "description": "Read an image from a workspace path or HTTP URL and attach it to the next LLM turn as multimodal content.\n\nUse this before reasoning about image content, UI screenshots, diagrams, visual errors, or generated assets. Prefer workspace-relative paths unless outside-workspace access is explicitly enabled.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Image path (workspace-relative by default; absolute path allowed when outside-workspace access is enabled) or http(s) image URL."}
                },
                "required": ["path"]
            }
        }
    })
}
