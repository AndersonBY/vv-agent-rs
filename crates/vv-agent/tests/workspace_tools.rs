use std::collections::BTreeMap;

use serde_json::{json, Value};
use vv_agent::{build_default_registry, ToolCall, ToolContext, ToolResultStatus};

#[test]
fn default_workspace_tools_can_write_read_replace_and_list_files() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());

    let write = registry
        .execute(
            &ToolCall::new(
                "write_1",
                "write_file",
                BTreeMap::from([
                    ("path".to_string(), json!("notes.md")),
                    ("content".to_string(), json!("hello world")),
                ]),
            ),
            &mut context,
        )
        .expect("write tool");
    assert_eq!(write.status, ToolResultStatus::Success);

    let replace = registry
        .execute(
            &ToolCall::new(
                "replace_1",
                "file_str_replace",
                BTreeMap::from([
                    ("path".to_string(), json!("notes.md")),
                    ("old_str".to_string(), json!("world")),
                    ("new_str".to_string(), json!("agent")),
                ]),
            ),
            &mut context,
        )
        .expect("replace tool");
    assert_eq!(replace.status, ToolResultStatus::Success);

    let read = registry
        .execute(
            &ToolCall::new(
                "read_1",
                "read_file",
                BTreeMap::from([("path".to_string(), json!("notes.md"))]),
            ),
            &mut context,
        )
        .expect("read tool");
    assert!(read.content.contains("hello agent"));

    let list = registry
        .execute(
            &ToolCall::new("list_1", "list_files", BTreeMap::new()),
            &mut context,
        )
        .expect("list tool");
    assert!(list.content.contains("notes.md"));
}

#[test]
fn list_files_skips_common_dependency_roots_by_default() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::create_dir_all(workspace.path().join("src")).expect("src dir");
    std::fs::create_dir_all(workspace.path().join("node_modules/pkg")).expect("node_modules dir");
    std::fs::write(workspace.path().join("src/main.rs"), "fn main() {}").expect("src file");
    std::fs::write(workspace.path().join("node_modules/pkg/a.js"), "a").expect("ignored file");

    let list = registry
        .execute(
            &ToolCall::new("list_1", "list_files", BTreeMap::new()),
            &mut context,
        )
        .expect("list tool");

    assert!(list.content.contains("src/main.rs"));
    assert!(!list.content.contains("node_modules/pkg/a.js"));
    assert!(list.content.contains("ignored_roots"));
}

#[test]
fn file_info_reports_file_metadata() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(workspace.path().join("notes.md"), "hello").expect("file");

    let info = registry
        .execute(
            &ToolCall::new(
                "info_1",
                "file_info",
                BTreeMap::from([("path".to_string(), json!("notes.md"))]),
            ),
            &mut context,
        )
        .expect("file_info tool");

    assert_eq!(info.status, ToolResultStatus::Success);
    assert!(info.content.contains("\"exists\":true"));
    assert!(info.content.contains("\"size\":5"));
    assert!(info.content.contains("\"suffix\":\"md\""));
}

#[test]
fn workspace_file_tools_reject_paths_outside_workspace_by_default() {
    let workspace = tempfile::tempdir().expect("workspace");
    let outside = tempfile::tempdir().expect("outside");
    let outside_file = outside.path().join("escape.txt");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());

    let write = registry
        .execute(
            &ToolCall::new(
                "write_escape",
                "write_file",
                BTreeMap::from([
                    ("path".to_string(), json!(outside_file)),
                    ("content".to_string(), json!("escaped")),
                ]),
            ),
            &mut context,
        )
        .expect("write tool");

    assert_eq!(write.status, ToolResultStatus::Error);
    assert_eq!(write.error_code.as_deref(), Some("path_escapes_workspace"));
    assert!(!outside_file.exists());

    let read = registry
        .execute(
            &ToolCall::new(
                "read_escape",
                "read_file",
                BTreeMap::from([("path".to_string(), json!(outside_file))]),
            ),
            &mut context,
        )
        .expect("read tool");

    assert_eq!(read.status, ToolResultStatus::Error);
    assert_eq!(read.error_code.as_deref(), Some("path_escapes_workspace"));
}

#[test]
fn workspace_file_tools_can_access_outside_paths_when_metadata_allows_it() {
    let workspace = tempfile::tempdir().expect("workspace");
    let outside = tempfile::tempdir().expect("outside");
    let outside_file = outside.path().join("allowed.txt");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    context.metadata.insert(
        "allow_outside_workspace_paths".to_string(),
        Value::Bool(true),
    );

    let write = registry
        .execute(
            &ToolCall::new(
                "write_allowed",
                "write_file",
                BTreeMap::from([
                    ("path".to_string(), json!(outside_file)),
                    ("content".to_string(), json!("allowed")),
                ]),
            ),
            &mut context,
        )
        .expect("write tool");
    assert_eq!(write.status, ToolResultStatus::Success);

    let read = registry
        .execute(
            &ToolCall::new(
                "read_allowed",
                "read_file",
                BTreeMap::from([("path".to_string(), json!(outside_file))]),
            ),
            &mut context,
        )
        .expect("read tool");
    assert_eq!(read.status, ToolResultStatus::Success);
    assert!(read.content.contains("allowed"));
}

#[test]
fn read_file_returns_file_info_when_requested_slice_exceeds_limits() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    let large_content = (0..2_001)
        .map(|line| format!("line-{line}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(workspace.path().join("large.txt"), large_content).expect("large file");

    let result = registry
        .execute(
            &ToolCall::new(
                "read_large",
                "read_file",
                BTreeMap::from([("path".to_string(), json!("large.txt"))]),
            ),
            &mut context,
        )
        .expect("read tool");

    assert_eq!(result.status, ToolResultStatus::Success);
    let payload: Value = serde_json::from_str(&result.content).expect("payload");
    assert_eq!(payload["content"], Value::Null);
    assert_eq!(payload["file_info"]["total_lines"], 2_001);
    assert_eq!(payload["limits"]["max_lines"], 2_000);
    assert_eq!(payload["suggested_range"]["start_line"], 1);
    assert_eq!(payload["suggested_range"]["end_line"], 2_000);
    assert!(payload["message"]
        .as_str()
        .expect("message")
        .contains("exceeds limits"));
}
