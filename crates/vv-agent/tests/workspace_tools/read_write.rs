use super::*;

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

#[test]
fn read_file_returns_file_info_when_char_limit_exceeded() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(workspace.path().join("chars.txt"), "a".repeat(50_001))
        .expect("large char file");

    let result = registry
        .execute(
            &ToolCall::new(
                "read_large_chars",
                "read_file",
                BTreeMap::from([("path".to_string(), json!("chars.txt"))]),
            ),
            &mut context,
        )
        .expect("read tool");

    assert_eq!(result.status, ToolResultStatus::Success);
    let payload: Value = serde_json::from_str(&result.content).expect("payload");
    assert_eq!(payload["content"], Value::Null);
    assert_eq!(payload["file_info"]["total_chars"], 50_001);
    assert_eq!(payload["requested"]["char_count"], 50_001);
    assert_eq!(payload["limits"]["max_chars"], 50_000);
    assert!(payload["message"]
        .as_str()
        .expect("message")
        .contains("exceeds limits"));
}

#[test]
fn read_file_accepts_string_line_numbers() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(workspace.path().join("notes.txt"), "alpha\nbeta\ngamma").expect("file");

    let result = registry
        .execute(
            &ToolCall::new(
                "read_string_lines",
                "read_file",
                BTreeMap::from([
                    ("path".to_string(), json!("notes.txt")),
                    ("start_line".to_string(), json!("2")),
                    ("end_line".to_string(), json!("2")),
                    ("show_line_numbers".to_string(), json!(true)),
                ]),
            ),
            &mut context,
        )
        .expect("read tool");

    let payload: Value = serde_json::from_str(&result.content).expect("payload");
    assert_eq!(payload["start_line"], 2);
    assert_eq!(payload["end_line"], 2);
    assert_eq!(payload["content"], "2: beta");
}

#[test]
fn read_file_preserves_requested_start_line_for_empty_out_of_range_slice() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(workspace.path().join("short.txt"), "one\ntwo").expect("file");

    let result = registry
        .execute(
            &ToolCall::new(
                "read_empty_range",
                "read_file",
                BTreeMap::from([
                    ("path".to_string(), json!("short.txt")),
                    ("start_line".to_string(), json!(10)),
                ]),
            ),
            &mut context,
        )
        .expect("read tool");

    assert_eq!(result.status, ToolResultStatus::Success);
    let payload: Value = serde_json::from_str(&result.content).expect("payload");
    assert_eq!(payload["start_line"], 10);
    assert_eq!(payload["end_line"], 9);
    assert_eq!(payload["content"], "");
}

#[test]
fn read_file_uses_json_truthiness_for_show_line_numbers() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(workspace.path().join("notes.txt"), "alpha\nbeta").expect("file");

    let result = registry
        .execute(
            &ToolCall::new(
                "read_truthy_show_lines",
                "read_file",
                BTreeMap::from([
                    ("path".to_string(), json!("notes.txt")),
                    ("show_line_numbers".to_string(), json!("false")),
                ]),
            ),
            &mut context,
        )
        .expect("read tool");

    let payload: Value = serde_json::from_str(&result.content).expect("payload");
    assert_eq!(payload["show_line_numbers"], true);
    assert_eq!(payload["content"], "1: alpha\n2: beta");
}

#[test]
fn write_file_uses_json_truthiness_for_append_flags() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(workspace.path().join("notes.txt"), "alpha").expect("file");

    let result = registry
        .execute(
            &ToolCall::new(
                "write_truthy_append",
                "write_file",
                BTreeMap::from([
                    ("path".to_string(), json!("notes.txt")),
                    ("content".to_string(), json!("beta")),
                    ("append".to_string(), json!("false")),
                    ("leading_newline".to_string(), json!("false")),
                    ("trailing_newline".to_string(), json!("false")),
                ]),
            ),
            &mut context,
        )
        .expect("write tool");

    let payload: Value = serde_json::from_str(&result.content).expect("payload");
    assert_eq!(payload["append"], true);
    assert_eq!(payload["leading_newline"], true);
    assert_eq!(payload["trailing_newline"], true);
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("notes.txt")).expect("notes"),
        "alpha\nbeta\n"
    );
}

#[test]
fn write_file_overwrite_existing_requires_read_baseline() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(workspace.path().join("overwrite.txt"), "old").expect("file");

    let rejected = registry
        .execute(
            &ToolCall::new(
                "overwrite_without_read",
                "write_file",
                BTreeMap::from([
                    ("path".to_string(), json!("overwrite.txt")),
                    ("content".to_string(), json!("new")),
                ]),
            ),
            &mut context,
        )
        .expect("write_file");
    assert_eq!(rejected.status, ToolResultStatus::Error);
    assert_eq!(rejected.error_code.as_deref(), Some("file_not_read"));

    registry
        .execute(
            &ToolCall::new(
                "read_before_overwrite",
                "read_file",
                BTreeMap::from([("path".to_string(), json!("overwrite.txt"))]),
            ),
            &mut context,
        )
        .expect("read_file");
    let written = registry
        .execute(
            &ToolCall::new(
                "overwrite_after_read",
                "write_file",
                BTreeMap::from([
                    ("path".to_string(), json!("overwrite.txt")),
                    ("content".to_string(), json!("new")),
                ]),
            ),
            &mut context,
        )
        .expect("write_file");
    assert_eq!(written.status, ToolResultStatus::Success);
    assert_eq!(written.metadata["operation"], json!("write_file"));
    assert_eq!(written.metadata["changed_files"], json!(["overwrite.txt"]));
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("overwrite.txt")).expect("file"),
        "new"
    );
}

#[test]
fn write_file_overwrite_rejects_file_changed_since_read() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(workspace.path().join("stale.txt"), "old").expect("file");
    registry
        .execute(
            &ToolCall::new(
                "read_stale",
                "read_file",
                BTreeMap::from([("path".to_string(), json!("stale.txt"))]),
            ),
            &mut context,
        )
        .expect("read_file");
    std::fs::write(workspace.path().join("stale.txt"), "user change").expect("file");

    let result = registry
        .execute(
            &ToolCall::new(
                "overwrite_stale",
                "write_file",
                BTreeMap::from([
                    ("path".to_string(), json!("stale.txt")),
                    ("content".to_string(), json!("new")),
                ]),
            ),
            &mut context,
        )
        .expect("write_file");

    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(
        result.error_code.as_deref(),
        Some("file_changed_since_read")
    );
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("stale.txt")).expect("file"),
        "user change"
    );
}
