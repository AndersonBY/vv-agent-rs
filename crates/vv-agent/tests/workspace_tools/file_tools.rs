use super::*;

#[test]
fn default_workspace_tools_can_write_read_edit_and_find_files() {
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

    let read_before_edit = registry
        .execute(
            &ToolCall::new(
                "read_before_edit",
                "read_file",
                BTreeMap::from([("path".to_string(), json!("notes.md"))]),
            ),
            &mut context,
        )
        .expect("read before edit");
    assert_eq!(read_before_edit.status, ToolResultStatus::Success);

    let edit = registry
        .execute(
            &ToolCall::new(
                "edit_1",
                "edit_file",
                BTreeMap::from([
                    ("path".to_string(), json!("notes.md")),
                    ("old_string".to_string(), json!("world")),
                    ("new_string".to_string(), json!("agent")),
                ]),
            ),
            &mut context,
        )
        .expect("edit tool");
    assert_eq!(edit.status, ToolResultStatus::Success);

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
            &ToolCall::new("list_1", "find_files", BTreeMap::new()),
            &mut context,
        )
        .expect("list tool");
    assert!(list.content.contains("notes.md"));
}

#[test]
fn edit_file_replaces_legacy_replace_tool_in_default_tools() {
    let registry = build_default_registry();

    assert!(registry.has_tool("edit_file"));
    assert!(!registry.has_tool(&format!("file_str_{}", "replace")));

    let schema = registry.get_schema("edit_file").expect("edit_file schema");
    let function = schema["function"].as_object().expect("function schema");
    assert_eq!(function["name"], json!("edit_file"));
    let parameters = function["parameters"].as_object().expect("parameters");
    assert_eq!(
        parameters["required"],
        json!(["path", "old_string", "new_string"])
    );
    let properties = parameters["properties"].as_object().expect("properties");
    let keys = properties.keys().cloned().collect::<Vec<_>>();
    assert_eq!(
        keys,
        vec!["new_string", "old_string", "path", "replace_all"]
    );
}

#[test]
fn legacy_replace_tool_name_is_removed() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    let legacy_tool_name = format!("file_str_{}", "replace");

    let result = registry.execute(
        &ToolCall::new(
            "removed_replace",
            legacy_tool_name,
            BTreeMap::from([
                ("path".to_string(), json!("edit.txt")),
                (format!("old_{}", "str"), json!("a")),
                (format!("new_{}", "str"), json!("b")),
            ]),
        ),
        &mut context,
    );

    assert!(result.is_err());
}

#[test]
fn edit_file_rejects_legacy_argument_names() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(workspace.path().join("legacy.txt"), "hello").expect("file");
    registry
        .execute(
            &ToolCall::new(
                "read_legacy",
                "read_file",
                BTreeMap::from([("path".to_string(), json!("legacy.txt"))]),
            ),
            &mut context,
        )
        .expect("read_file");

    let result = registry
        .execute(
            &ToolCall::new(
                "edit_legacy",
                "edit_file",
                BTreeMap::from([
                    ("path".to_string(), json!("legacy.txt")),
                    (format!("old_{}", "str"), json!("hello")),
                    (format!("new_{}", "str"), json!("hi")),
                ]),
            ),
            &mut context,
        )
        .expect("edit_file");

    let payload: Value = serde_json::from_str(&result.content).expect("payload");
    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(result.error_code.as_deref(), Some("invalid_arguments"));
    assert_eq!(payload["error_code"], json!("invalid_arguments"));
    assert!(payload["error"]
        .as_str()
        .expect("message")
        .contains("old_string"));
}

#[test]
fn write_file_coerces_scalar_content() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());

    let result = registry
        .execute(
            &ToolCall::new(
                "write_number_content",
                "write_file",
                BTreeMap::from([
                    ("path".to_string(), json!("number.txt")),
                    ("content".to_string(), json!(123)),
                ]),
            ),
            &mut context,
        )
        .expect("write_file");

    assert_eq!(result.status, ToolResultStatus::Success);
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("number.txt")).expect("file"),
        "123"
    );
}

#[test]
fn workspace_file_tools_coerce_scalar_path_and_glob() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());

    let write = registry
        .execute(
            &ToolCall::new(
                "write_scalar_path",
                "write_file",
                BTreeMap::from([
                    ("path".to_string(), json!(123)),
                    ("content".to_string(), json!("one")),
                ]),
            ),
            &mut context,
        )
        .expect("write_file");

    assert_eq!(write.status, ToolResultStatus::Success);
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("123")).expect("scalar path file"),
        "one"
    );

    let read = registry
        .execute(
            &ToolCall::new(
                "read_scalar_path",
                "read_file",
                BTreeMap::from([("path".to_string(), json!(123))]),
            ),
            &mut context,
        )
        .expect("read_file");
    assert_eq!(read.status, ToolResultStatus::Success);
    assert!(read.content.contains("\"path\":\"123\""));
    assert!(read.content.contains("\"content\":\"one\""));

    let edit = registry
        .execute(
            &ToolCall::new(
                "edit_scalar_path",
                "edit_file",
                BTreeMap::from([
                    ("path".to_string(), json!(123)),
                    ("old_string".to_string(), json!("one")),
                    ("new_string".to_string(), json!("two")),
                ]),
            ),
            &mut context,
        )
        .expect("edit_file");
    assert_eq!(edit.status, ToolResultStatus::Success);

    let info = registry
        .execute(
            &ToolCall::new(
                "info_scalar_path",
                "file_info",
                BTreeMap::from([("path".to_string(), json!(123))]),
            ),
            &mut context,
        )
        .expect("file_info");
    assert_eq!(info.status, ToolResultStatus::Success);
    assert!(info.content.contains("\"path\":\"123\""));

    std::fs::create_dir_all(workspace.path().join("456")).expect("number dir");
    std::fs::write(workspace.path().join("456/123"), "number glob").expect("number glob file");
    std::fs::write(workspace.path().join("456/other.txt"), "other").expect("other file");
    let list = registry
        .execute(
            &ToolCall::new(
                "list_scalar_path_glob",
                "find_files",
                BTreeMap::from([
                    ("path".to_string(), json!(456)),
                    ("glob".to_string(), json!(123)),
                ]),
            ),
            &mut context,
        )
        .expect("find_files");
    let list_payload: Value = serde_json::from_str(&list.content).expect("list payload");
    assert_eq!(list_payload["files"], json!(["456/123"]));
}

#[test]
fn edit_file_requires_full_read_before_edit() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(workspace.path().join("edit.txt"), "hello").expect("file");

    let result = registry
        .execute(
            &ToolCall::new(
                "edit_without_read",
                "edit_file",
                BTreeMap::from([
                    ("path".to_string(), json!("edit.txt")),
                    ("old_string".to_string(), json!("hello")),
                    ("new_string".to_string(), json!("hi")),
                ]),
            ),
            &mut context,
        )
        .expect("edit_file");

    let payload: Value = serde_json::from_str(&result.content).expect("payload");
    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(result.error_code.as_deref(), Some("file_not_read"));
    assert_eq!(payload["error_code"], json!("file_not_read"));
    assert!(payload["error"]
        .as_str()
        .expect("message")
        .contains("read_file"));
}

#[test]
fn edit_file_rejects_partial_read_baseline() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(workspace.path().join("partial.txt"), "line1\nline2\nline3").expect("file");
    registry
        .execute(
            &ToolCall::new(
                "read_partial",
                "read_file",
                BTreeMap::from([
                    ("path".to_string(), json!("partial.txt")),
                    ("start_line".to_string(), json!(2)),
                    ("end_line".to_string(), json!(2)),
                ]),
            ),
            &mut context,
        )
        .expect("read_file");

    let result = registry
        .execute(
            &ToolCall::new(
                "edit_after_partial",
                "edit_file",
                BTreeMap::from([
                    ("path".to_string(), json!("partial.txt")),
                    ("old_string".to_string(), json!("line2")),
                    ("new_string".to_string(), json!("changed")),
                ]),
            ),
            &mut context,
        )
        .expect("edit_file");

    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(result.error_code.as_deref(), Some("file_not_read"));
}

#[test]
fn edit_file_rejects_file_changed_since_read() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(workspace.path().join("changed.txt"), "hello").expect("file");
    registry
        .execute(
            &ToolCall::new(
                "read_changed",
                "read_file",
                BTreeMap::from([("path".to_string(), json!("changed.txt"))]),
            ),
            &mut context,
        )
        .expect("read_file");
    std::fs::write(workspace.path().join("changed.txt"), "hello from user").expect("file");

    let result = registry
        .execute(
            &ToolCall::new(
                "edit_changed",
                "edit_file",
                BTreeMap::from([
                    ("path".to_string(), json!("changed.txt")),
                    ("old_string".to_string(), json!("hello")),
                    ("new_string".to_string(), json!("hi")),
                ]),
            ),
            &mut context,
        )
        .expect("edit_file");

    let payload: Value = serde_json::from_str(&result.content).expect("payload");
    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(
        result.error_code.as_deref(),
        Some("file_changed_since_read")
    );
    assert_eq!(payload["error_code"], json!("file_changed_since_read"));
}

#[test]
fn edit_file_requires_unique_match_by_default_and_supports_replace_all() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(workspace.path().join("duplicate.txt"), "one one one").expect("file");
    registry
        .execute(
            &ToolCall::new(
                "read_duplicate",
                "read_file",
                BTreeMap::from([("path".to_string(), json!("duplicate.txt"))]),
            ),
            &mut context,
        )
        .expect("read_file");

    let ambiguous = registry
        .execute(
            &ToolCall::new(
                "edit_duplicate_ambiguous",
                "edit_file",
                BTreeMap::from([
                    ("path".to_string(), json!("duplicate.txt")),
                    ("old_string".to_string(), json!("one")),
                    ("new_string".to_string(), json!("two")),
                ]),
            ),
            &mut context,
        )
        .expect("edit_file");
    assert_eq!(ambiguous.status, ToolResultStatus::Error);
    assert_eq!(
        ambiguous.error_code.as_deref(),
        Some("old_string_not_unique")
    );

    let replace_all = registry
        .execute(
            &ToolCall::new(
                "edit_duplicate_all",
                "edit_file",
                BTreeMap::from([
                    ("path".to_string(), json!("duplicate.txt")),
                    ("old_string".to_string(), json!("one")),
                    ("new_string".to_string(), json!("two")),
                    ("replace_all".to_string(), json!(true)),
                ]),
            ),
            &mut context,
        )
        .expect("edit_file");
    let payload: Value = serde_json::from_str(&replace_all.content).expect("payload");
    assert_eq!(replace_all.status, ToolResultStatus::Success);
    assert_eq!(payload["replaced_count"], 3);
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("duplicate.txt")).expect("file"),
        "two two two"
    );
}

#[test]
fn edit_file_success_returns_metadata_and_preserves_crlf() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(
        workspace.path().join("crlf.txt"),
        b"first\r\nsecond\r\nthird\r\n",
    )
    .expect("file");
    registry
        .execute(
            &ToolCall::new(
                "read_crlf",
                "read_file",
                BTreeMap::from([("path".to_string(), json!("crlf.txt"))]),
            ),
            &mut context,
        )
        .expect("read_file");

    let result = registry
        .execute(
            &ToolCall::new(
                "edit_crlf",
                "edit_file",
                BTreeMap::from([
                    ("path".to_string(), json!("crlf.txt")),
                    ("old_string".to_string(), json!("second\nthird")),
                    ("new_string".to_string(), json!("SECOND\nTHIRD")),
                ]),
            ),
            &mut context,
        )
        .expect("edit_file");

    let payload: Value = serde_json::from_str(&result.content).expect("payload");
    assert_eq!(result.status, ToolResultStatus::Success);
    assert_eq!(payload["replaced_count"], 1);
    assert_eq!(result.metadata["operation"], json!("edit_file"));
    assert_eq!(result.metadata["line_ending"], json!("crlf"));
    assert_eq!(result.metadata["changed_files"], json!(["crlf.txt"]));
    assert!(result.metadata["diff"]
        .as_str()
        .expect("diff")
        .contains("-second"));
    assert!(result.metadata["diff"]
        .as_str()
        .expect("diff")
        .contains("+SECOND"));
    assert_eq!(
        std::fs::read(workspace.path().join("crlf.txt")).expect("file"),
        b"first\r\nSECOND\r\nTHIRD\r\n"
    );
}
