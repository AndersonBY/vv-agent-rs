use std::collections::BTreeMap;

use serde_json::json;
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
