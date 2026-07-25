use serde_json::{json, Value};

const READ_IMAGE_DESCRIPTION: &str = "Attach a supported workspace image or HTTP image URL to the next model turn. Local files remain subject to workspace policy, format checks, and the inline size limit.";

pub(super) fn read_image_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "read_image",
            "description": READ_IMAGE_DESCRIPTION,
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Image path or URL to attach."}
                },
                "required": ["path"]
            }
        }
    })
}
