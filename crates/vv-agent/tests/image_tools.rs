use std::collections::BTreeMap;

use serde_json::{json, Value};
use vv_agent::{build_default_registry, ToolCall, ToolContext, ToolResultStatus};

const PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x60, 0x00, 0x00, 0x00,
    0x02, 0x00, 0x01, 0xe2, 0x21, 0xbc, 0x33, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];

#[test]
fn read_image_from_workspace_file_returns_inline_image_url() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(workspace.path().join("img.png"), PNG_1X1).expect("image");

    let result = registry
        .execute(
            &ToolCall::new(
                "img_1",
                "read_image",
                BTreeMap::from([("path".to_string(), json!("img.png"))]),
            ),
            &mut context,
        )
        .expect("read_image");

    assert_eq!(result.status, ToolResultStatus::Success);
    assert_eq!(result.image_path.as_deref(), Some("img.png"));
    assert!(result
        .image_url
        .as_deref()
        .expect("image url")
        .starts_with("data:image/png;base64,"));
    let payload: Value = serde_json::from_str(&result.content).expect("payload");
    assert_eq!(payload["source"], "workspace");
    assert_eq!(payload["inline_transport"], true);
}

#[test]
fn read_image_from_url_attaches_original_url() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());

    let result = registry
        .execute(
            &ToolCall::new(
                "img_2",
                "read_image",
                BTreeMap::from([("path".to_string(), json!("https://example.com/a.png"))]),
            ),
            &mut context,
        )
        .expect("read_image");

    assert_eq!(result.status, ToolResultStatus::Success);
    assert_eq!(
        result.image_url.as_deref(),
        Some("https://example.com/a.png")
    );
}

#[test]
fn read_image_rejects_non_string_path() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());

    let result = registry
        .execute(
            &ToolCall::new(
                "img_scalar_path",
                "read_image",
                BTreeMap::from([("path".to_string(), json!(123))]),
            ),
            &mut context,
        )
        .expect("read_image");

    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(result.error_code.as_deref(), Some("invalid_tool_arguments"));
    let payload: Value = serde_json::from_str(&result.content).expect("payload");
    assert_eq!(payload["error_code"], "invalid_tool_arguments");
}

#[test]
fn read_image_rejects_unsupported_extension() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(workspace.path().join("x.txt"), "not image").expect("text");

    let result = registry
        .execute(
            &ToolCall::new(
                "img_3",
                "read_image",
                BTreeMap::from([("path".to_string(), json!("x.txt"))]),
            ),
            &mut context,
        )
        .expect("read_image");

    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(
        result.error_code.as_deref(),
        Some("unsupported_image_format")
    );
}
