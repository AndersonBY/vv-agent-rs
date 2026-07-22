use super::*;

#[test]
fn read_file_counts_unicode_characters_and_preserves_result_contract() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    let content = "中".repeat(20_000);
    std::fs::write(workspace.path().join("cjk.txt"), &content).expect("cjk file");

    let result = registry
        .execute(
            &ToolCall::new(
                "read_cjk",
                "read_file",
                BTreeMap::from([("path".to_string(), json!("cjk.txt"))]),
            ),
            &mut context,
        )
        .expect("read_file");
    let payload: Value = serde_json::from_str(&result.content).expect("payload");

    assert_eq!(result.status, ToolResultStatus::Success);
    assert_eq!(result.directive, vv_agent::ToolDirective::Continue);
    assert_eq!(result.error_code, None);
    assert!(result.metadata.is_empty());
    assert_eq!(
        payload,
        json!({
            "path": "cjk.txt",
            "start_line": 1,
            "end_line": 1,
            "show_line_numbers": false,
            "content": content,
        })
    );
}

#[test]
fn read_file_too_large_counts_unicode_characters() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(workspace.path().join("large-cjk.txt"), "中".repeat(50_001))
        .expect("large cjk file");

    let result = registry
        .execute(
            &ToolCall::new(
                "read_large_cjk",
                "read_file",
                BTreeMap::from([("path".to_string(), json!("large-cjk.txt"))]),
            ),
            &mut context,
        )
        .expect("read_file");
    let payload: Value = serde_json::from_str(&result.content).expect("payload");

    assert_eq!(result.status, ToolResultStatus::Success);
    assert_eq!(result.directive, vv_agent::ToolDirective::Continue);
    assert_eq!(result.error_code, None);
    assert!(result.metadata.is_empty());
    assert_eq!(payload["content"], Value::Null);
    assert_eq!(
        payload["file_info"],
        json!({"total_lines": 1, "total_chars": 50_001})
    );
    assert_eq!(
        payload["requested"],
        json!({"line_count": 1, "char_count": 50_001})
    );
}

#[test]
fn read_file_validation_and_not_found_errors_are_structured() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());

    let missing_path = registry
        .execute(
            &ToolCall::new("read_missing_path", "read_file", BTreeMap::new()),
            &mut context,
        )
        .expect("read_file validation");
    let missing_payload: Value =
        serde_json::from_str(&missing_path.content).expect("missing payload");
    assert_eq!(missing_path.status, ToolResultStatus::Error);
    assert_eq!(missing_path.directive, vv_agent::ToolDirective::Continue);
    assert_eq!(
        missing_path.error_code.as_deref(),
        Some("invalid_tool_arguments")
    );
    assert_eq!(
        missing_path.metadata,
        BTreeMap::from([
            ("error_code".to_string(), json!("invalid_tool_arguments"),),
            ("issue_count".to_string(), json!(1)),
        ])
    );
    assert_eq!(
        missing_payload,
        json!({
            "ok": false,
            "error": "Tool arguments do not match the declared schema",
            "error_code": "invalid_tool_arguments",
            "issues": [{
                "instance_path": "",
                "schema_path": "/required",
                "rule": "required",
            }],
        })
    );

    let not_found = registry
        .execute(
            &ToolCall::new(
                "read_not_found",
                "read_file",
                BTreeMap::from([("path".to_string(), json!("missing.txt"))]),
            ),
            &mut context,
        )
        .expect("read_file not found");
    let not_found_payload: Value =
        serde_json::from_str(&not_found.content).expect("not found payload");
    assert_eq!(not_found.status, ToolResultStatus::Error);
    assert_eq!(not_found.directive, vv_agent::ToolDirective::Continue);
    assert_eq!(not_found.error_code.as_deref(), Some("file_not_found"));
    assert_eq!(
        not_found.metadata,
        BTreeMap::from([
            ("error_code".to_string(), json!("file_not_found")),
            ("path".to_string(), json!("missing.txt")),
        ])
    );
    assert_eq!(not_found_payload["ok"], false);
    assert_eq!(not_found_payload["error_code"], "file_not_found");
    assert_eq!(not_found_payload["path"], "missing.txt");
}

#[test]
fn write_file_reports_utf8_bytes_and_compatible_unicode_chars() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());

    let result = registry
        .execute(
            &ToolCall::new(
                "write_cjk",
                "write_file",
                BTreeMap::from([
                    ("path".to_string(), json!("written.txt")),
                    ("content".to_string(), json!("中文")),
                ]),
            ),
            &mut context,
        )
        .expect("write_file");
    let payload: Value = serde_json::from_str(&result.content).expect("payload");

    assert_eq!(result.status, ToolResultStatus::Success);
    assert_eq!(result.directive, vv_agent::ToolDirective::Continue);
    assert_eq!(result.error_code, None);
    assert_eq!(payload["written_bytes"], 6);
    assert_eq!(payload["written_chars"], 2);
    assert_eq!(
        result.metadata,
        BTreeMap::from([
            ("append".to_string(), json!(false)),
            ("changed_files".to_string(), json!(["written.txt"])),
            ("operation".to_string(), json!("write_file")),
        ])
    );
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
fn read_and_write_reject_schema_invalid_types() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    let cases = [
        (
            "read_file",
            BTreeMap::from([
                ("path".to_string(), json!("notes.txt")),
                ("start_line".to_string(), json!("2")),
            ]),
            "/start_line",
        ),
        (
            "read_file",
            BTreeMap::from([
                ("path".to_string(), json!("notes.txt")),
                ("show_line_numbers".to_string(), json!("false")),
            ]),
            "/show_line_numbers",
        ),
        (
            "write_file",
            BTreeMap::from([
                ("path".to_string(), json!("notes.txt")),
                ("content".to_string(), json!("beta")),
                ("append".to_string(), json!("false")),
            ]),
            "/append",
        ),
    ];

    for (tool_name, arguments, instance_path) in cases {
        let result = registry
            .execute(
                &ToolCall::new(format!("{tool_name}_invalid_type"), tool_name, arguments),
                &mut context,
            )
            .expect("tool validation");
        let payload: Value = serde_json::from_str(&result.content).expect("payload");
        assert_eq!(result.status, ToolResultStatus::Error);
        assert_eq!(result.error_code.as_deref(), Some("invalid_tool_arguments"));
        assert_eq!(payload["issues"][0]["instance_path"], instance_path);
        assert_eq!(payload["issues"][0]["rule"], "type");
    }
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
