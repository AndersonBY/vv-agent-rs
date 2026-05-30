use super::*;

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

    let replace = registry
        .execute(
            &ToolCall::new(
                "replace_scalar_path",
                "file_str_replace",
                BTreeMap::from([
                    ("path".to_string(), json!(123)),
                    ("old_str".to_string(), json!("one")),
                    ("new_str".to_string(), json!("two")),
                ]),
            ),
            &mut context,
        )
        .expect("file_str_replace");
    assert_eq!(replace.status, ToolResultStatus::Success);

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
                "list_files",
                BTreeMap::from([
                    ("path".to_string(), json!(456)),
                    ("glob".to_string(), json!(123)),
                ]),
            ),
            &mut context,
        )
        .expect("list_files");
    let list_payload: Value = serde_json::from_str(&list.content).expect("list payload");
    assert_eq!(list_payload["files"], json!(["456/123"]));
}

#[test]
fn file_str_replace_coerces_scalar_text_arguments() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(workspace.path().join("numbers.txt"), "1 1").expect("file");

    let result = registry
        .execute(
            &ToolCall::new(
                "replace_scalar_text",
                "file_str_replace",
                BTreeMap::from([
                    ("path".to_string(), json!("numbers.txt")),
                    ("old_str".to_string(), json!(1)),
                    ("new_str".to_string(), json!(2)),
                ]),
            ),
            &mut context,
        )
        .expect("file_str_replace");

    assert_eq!(result.status, ToolResultStatus::Success);
    let payload: Value = serde_json::from_str(&result.content).expect("payload");
    assert_eq!(payload["replaced_count"], 1);
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("numbers.txt")).expect("file"),
        "2 1"
    );
}

#[test]
fn file_str_replace_accepts_string_max_replacements() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(workspace.path().join("edit.txt"), "one one one").expect("file");

    let result = registry
        .execute(
            &ToolCall::new(
                "replace_string_limit",
                "file_str_replace",
                BTreeMap::from([
                    ("path".to_string(), json!("edit.txt")),
                    ("old_str".to_string(), json!("one")),
                    ("new_str".to_string(), json!("two")),
                    ("max_replacements".to_string(), json!("2")),
                ]),
            ),
            &mut context,
        )
        .expect("file_str_replace");

    assert_eq!(result.status, ToolResultStatus::Success);
    let payload: Value = serde_json::from_str(&result.content).expect("payload");
    assert_eq!(payload["replaced_count"], 2);
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("edit.txt")).expect("file"),
        "two two one"
    );
}
