use std::path::Path;
use std::sync::Arc;

use base64::Engine as _;
use serde_json::json;

use crate::tools::base::{ToolContext, ToolSpec};
use crate::tools::common::{
    path_escapes_workspace_error, stringify_tool_arg, tool_error_with_code, tool_result,
};
use crate::types::{ToolArguments, ToolDirective, ToolExecutionResult, ToolResultStatus};

pub fn read_image(context: &mut ToolContext, arguments: &ToolArguments) -> ToolExecutionResult {
    let spec = read_image_tool();
    (spec.handler)(context, arguments)
}

pub(crate) fn read_image_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "read_image",
        "Read image from workspace path or HTTP URL for multimodal follow-up.",
        Arc::new(|context, arguments| {
            let raw_path = stringify_tool_arg(arguments.get("path"), "");
            let raw_path = raw_path.trim();
            if raw_path.is_empty() {
                return tool_error_with_code("`path` is required", "path_required");
            }
            if raw_path.starts_with("http://") || raw_path.starts_with("https://") {
                let payload = json!({
                    "status": "loaded",
                    "source": "url",
                    "image_url": raw_path,
                });
                let mut result = tool_result(
                    ToolResultStatus::Success,
                    payload,
                    None,
                    ToolDirective::Continue,
                );
                result.image_url = Some(raw_path.to_string());
                return result;
            }
            if let Err(error) = context.resolve_workspace_path(raw_path) {
                return path_escapes_workspace_error(error);
            }
            let backend = context.effective_workspace_backend();
            if !backend.exists(raw_path) || !backend.is_file(raw_path) {
                return tool_error_with_code(
                    format!("image file not found: {raw_path}"),
                    "image_not_found",
                );
            }
            let suffix = Path::new(raw_path)
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| format!(".{}", ext.to_ascii_lowercase()))
                .unwrap_or_default();
            let mime_type = match suffix.as_str() {
                ".jpg" | ".jpeg" => "image/jpeg",
                ".png" => "image/png",
                ".webp" => "image/webp",
                ".bmp" => "image/bmp",
                _ => {
                    return tool_error_with_code(
                        format!("unsupported image format: {suffix}"),
                        "unsupported_image_format",
                    )
                }
            };
            let bytes = match backend.read_bytes(raw_path) {
                Ok(bytes) => bytes,
                Err(error) => return tool_error_with_code(error.to_string(), "image_not_found"),
            };
            const MAX_INLINE_IMAGE_BYTES: usize = 5 * 1024 * 1024;
            if bytes.len() > MAX_INLINE_IMAGE_BYTES {
                return tool_result(
                    ToolResultStatus::Error,
                    json!({
                        "error": "image is too large for inline message transport",
                        "max_bytes": MAX_INLINE_IMAGE_BYTES,
                        "actual_bytes": bytes.len(),
                    }),
                    Some("image_too_large"),
                    ToolDirective::Continue,
                );
            }
            let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
            let image_url = format!("data:{mime_type};base64,{encoded}");
            let payload = json!({
                "status": "loaded",
                "source": "workspace",
                "image_path": raw_path,
                "mime_type": mime_type,
                "inline_transport": true,
            });
            let mut result = tool_result(
                ToolResultStatus::Success,
                payload,
                None,
                ToolDirective::Continue,
            );
            result.image_url = Some(image_url);
            result.image_path = Some(raw_path.to_string());
            result
        }),
    );
    if let Some(schema) = super::super::schemas::schema_for("read_image") {
        spec.schema = schema;
    }
    spec
}
