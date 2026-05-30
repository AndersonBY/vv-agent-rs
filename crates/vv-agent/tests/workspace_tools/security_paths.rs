use super::*;

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
    assert!(info.content.contains("\"suffix\":\".md\""));

    let payload: Value = serde_json::from_str(&info.content).expect("file_info payload");
    let modified_at = payload["modified_at"].as_str().expect("modified_at");
    assert!(
        chrono::DateTime::parse_from_rfc3339(modified_at).is_ok(),
        "modified_at should match UTC ISO format, got {modified_at:?}"
    );
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

#[cfg(unix)]
#[test]
fn workspace_file_tools_reject_symlink_escape_by_default() {
    let workspace = tempfile::tempdir().expect("workspace");
    let outside = tempfile::tempdir().expect("outside");
    let outside_file = outside.path().join("secret.txt");
    std::fs::write(&outside_file, "outside secret").expect("outside file");
    std::os::unix::fs::symlink(outside.path(), workspace.path().join("linked-outside"))
        .expect("symlink");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());

    let read = registry
        .execute(
            &ToolCall::new(
                "read_symlink_escape",
                "read_file",
                BTreeMap::from([("path".to_string(), json!("linked-outside/secret.txt"))]),
            ),
            &mut context,
        )
        .expect("read_file symlink escape");

    assert_eq!(read.status, ToolResultStatus::Error);
    assert_eq!(read.error_code.as_deref(), Some("path_escapes_workspace"));
    assert!(
        read.content.contains("Path escapes workspace"),
        "symlink escape should be rejected, got {}",
        read.content
    );
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
fn workspace_paths_expand_home() {
    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
        return;
    };
    let workspace = tempfile::tempdir().expect("workspace");
    let home_file = tempfile::NamedTempFile::new_in(&home).expect("home temp file");
    std::fs::write(home_file.path(), "home expanded").expect("write home temp file");
    let home_relative_path = format!(
        "~/{}",
        home_file
            .path()
            .file_name()
            .expect("home filename")
            .to_string_lossy()
    );

    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    context.metadata.insert(
        "allow_outside_workspace_paths".to_string(),
        Value::Bool(true),
    );

    let result = registry
        .execute(
            &ToolCall::new(
                "read_home",
                "read_file",
                BTreeMap::from([("path".to_string(), json!(home_relative_path))]),
            ),
            &mut context,
        )
        .expect("read_file");

    assert_eq!(result.status, ToolResultStatus::Success);
    assert_eq!(
        serde_json::from_str::<Value>(&result.content).expect("payload")["content"],
        "home expanded"
    );
}
#[test]
fn local_workspace_backend_enforces_root_and_reports_allowed_outside_paths() {
    let workspace = tempfile::tempdir().expect("workspace");
    let outside = tempfile::tempdir().expect("outside");
    let outside_file = outside.path().join("external.txt");
    std::fs::write(&outside_file, "external").expect("outside file");

    let local = LocalWorkspaceBackend::new(workspace.path());
    assert_eq!(
        local
            .read_text(outside_file.to_str().expect("outside path"))
            .expect_err("outside blocked")
            .kind(),
        ErrorKind::PermissionDenied
    );

    let mut allowed = LocalWorkspaceBackend::new(workspace.path());
    allowed.allow_outside_root = true;
    assert_eq!(
        allowed
            .read_text(outside_file.to_str().expect("outside path"))
            .expect("outside read"),
        "external"
    );
    assert_eq!(
        allowed
            .list_files(outside.path().to_str().expect("outside dir"), "**/*.txt")
            .expect("outside list"),
        vec![outside_file.to_string_lossy().to_string()]
    );
    assert_eq!(
        allowed
            .file_info(outside_file.to_str().expect("outside file"))
            .expect("outside info")
            .expect("outside exists")
            .path,
        outside_file.to_string_lossy().to_string()
    );
}
