use serde_json::{json, Value};

const READ_IMAGE_DESCRIPTION: &str = r#"Read image from workspace path or HTTP URL, then attach the image payload to the next LLM turn as multimodal content.

Read an image and attach it to the next LLM turn as multimodal content.

When to use:
- Use this before reasoning about image content.
- Before reasoning about image content, UI screenshots, diagrams, visual errors, generated assets, or visual regression evidence.
- Use this when text tools can only tell you that an image exists, but the Agent needs to inspect what is actually visible.
- Prefer workspace-relative paths for local artifacts unless outside-workspace access is explicitly enabled.

Supported inputs:
- Supported formats for workspace files: PNG, JPEG, WEBP, and BMP.
- Inline local image transport is limited to 5 MiB to protect the LLM request size.
- HTTP URLs are passed through as image URLs without downloading.

Returns:
- A multimodal attachment for the next model turn plus normalized source metadata.
- Structured errors for unsupported file types, oversized local images, missing paths, or paths outside the permitted workspace."#;

pub(super) fn read_image_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "read_image",
            "description": READ_IMAGE_DESCRIPTION,
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Image path (workspace-relative by default; absolute path allowed when outside-workspace access is enabled) or http(s) image URL. Non-string scalar values are coerced to text for Python compatibility."}
                },
                "required": ["path"]
            }
        }
    })
}
