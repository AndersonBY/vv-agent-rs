use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::sync::Arc;

use object_store::memory::InMemory;
use serde_json::{json, Value};
use vv_agent::workspace::{
    LocalWorkspaceBackend, MemoryWorkspaceBackend, S3WorkspaceBackend, WorkspaceBackend,
};
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
fn list_files_can_list_inside_ignored_root_when_targeted_directly() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::create_dir_all(workspace.path().join("node_modules/pkg")).expect("node_modules dir");
    std::fs::write(workspace.path().join("node_modules/pkg/a.js"), "a").expect("ignored file");

    let list = registry
        .execute(
            &ToolCall::new(
                "list_targeted_ignored_root",
                "list_files",
                BTreeMap::from([("path".to_string(), json!("node_modules"))]),
            ),
            &mut context,
        )
        .expect("list_files");
    let payload: Value = serde_json::from_str(&list.content).expect("list payload");

    assert_eq!(payload["files"], json!(["node_modules/pkg/a.js"]));
    assert!(payload.get("ignored_roots").is_none());
}

#[test]
fn list_files_combines_scan_limit_and_ignored_root_messages() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(workspace.path().join("a.txt"), "a").expect("a file");
    std::fs::write(workspace.path().join("b.txt"), "b").expect("b file");
    std::fs::write(workspace.path().join("c.txt"), "c").expect("c file");
    std::fs::create_dir_all(workspace.path().join("node_modules/pkg")).expect("ignored dir");
    std::fs::write(
        workspace.path().join("node_modules/pkg/ignored.js"),
        "ignored",
    )
    .expect("ignored file");

    let list = registry
        .execute(
            &ToolCall::new(
                "list_limited_with_ignored",
                "list_files",
                BTreeMap::from([
                    ("max_results".to_string(), json!(1)),
                    ("scan_limit".to_string(), json!(2)),
                ]),
            ),
            &mut context,
        )
        .expect("list_files");
    let payload: Value = serde_json::from_str(&list.content).expect("list payload");
    let message = payload["message"].as_str().expect("combined message");

    assert_eq!(payload["count_is_estimate"], json!(true));
    assert_eq!(payload["ignored_roots"], json!([{"path": "node_modules"}]));
    assert!(
        message.contains("Listing stopped early due to scan limit"),
        "missing scan-limit guidance: {message}"
    );
    assert!(
        message.contains("Common dependency/cache directories are summarized by default"),
        "missing ignored-root guidance: {message}"
    );
}

#[test]
fn list_files_empty_path_is_workspace_root() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::create_dir_all(workspace.path().join("src")).expect("src dir");
    std::fs::create_dir_all(workspace.path().join("node_modules/pkg")).expect("node_modules dir");
    std::fs::write(workspace.path().join("src/main.rs"), "fn main() {}").expect("src file");
    std::fs::write(workspace.path().join("node_modules/pkg/a.js"), "a").expect("ignored file");

    let list = registry
        .execute(
            &ToolCall::new(
                "list_empty_path",
                "list_files",
                BTreeMap::from([("path".to_string(), json!(""))]),
            ),
            &mut context,
        )
        .expect("list tool");

    assert!(list.content.contains("src/main.rs"));
    assert!(!list.content.contains("node_modules/pkg/a.js"));
    assert!(list.content.contains("ignored_roots"));
}

#[test]
fn list_files_non_local_backend_does_not_summarize_ignored_roots() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let backend = MemoryWorkspaceBackend::default();
    backend.mkdir("node_modules/pkg").expect("node_modules dir");
    backend
        .write_text("node_modules/pkg/a.js", "a", false)
        .expect("ignored root file");
    let mut context = ToolContext::new(workspace.path());
    context.workspace_backend = Arc::new(backend);

    let list = registry
        .execute(
            &ToolCall::new("list_memory_root", "list_files", BTreeMap::new()),
            &mut context,
        )
        .expect("list tool");
    let payload: Value = serde_json::from_str(&list.content).expect("list payload");

    assert_eq!(payload["files"], json!(["node_modules/pkg/a.js"]));
    assert!(payload.get("ignored_roots").is_none());
}

#[test]
fn list_files_excludes_hidden_by_default_and_can_include_them() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(workspace.path().join("visible.txt"), "visible").expect("visible file");
    std::fs::write(workspace.path().join(".hidden.txt"), "hidden").expect("hidden file");

    let default_list = registry
        .execute(
            &ToolCall::new("list_hidden_default", "list_files", BTreeMap::new()),
            &mut context,
        )
        .expect("default list");
    let default_payload: Value =
        serde_json::from_str(&default_list.content).expect("default payload");
    assert_eq!(default_payload["files"], json!(["visible.txt"]));

    let included = registry
        .execute(
            &ToolCall::new(
                "list_hidden_included",
                "list_files",
                BTreeMap::from([("include_hidden".to_string(), json!(true))]),
            ),
            &mut context,
        )
        .expect("included list");
    let included_payload: Value =
        serde_json::from_str(&included.content).expect("included payload");
    assert_eq!(
        included_payload["files"],
        json!([".hidden.txt", "visible.txt"])
    );
}

#[test]
fn list_files_reports_estimated_count_when_scan_limit_is_reached() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    for index in 0..40 {
        std::fs::write(workspace.path().join(format!("scan_{index:03}.txt")), "x")
            .expect("scan file");
    }

    let list = registry
        .execute(
            &ToolCall::new(
                "list_scan_limit",
                "list_files",
                BTreeMap::from([
                    ("max_results".to_string(), json!(10)),
                    ("scan_limit".to_string(), json!(12)),
                ]),
            ),
            &mut context,
        )
        .expect("list tool");
    let payload: Value = serde_json::from_str(&list.content).expect("list payload");

    assert_eq!(payload["returned_count"], 10);
    assert_eq!(payload["truncated"], true);
    assert_eq!(payload["count_is_estimate"], true);
    assert_eq!(payload["scan_limit"], 12);
}

#[test]
fn list_files_accepts_string_limits() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    for index in 0..5 {
        std::fs::write(workspace.path().join(format!("file_{index}.txt")), "x").expect("file");
    }

    let list = registry
        .execute(
            &ToolCall::new(
                "list_string_limits",
                "list_files",
                BTreeMap::from([
                    ("max_results".to_string(), json!("2")),
                    ("scan_limit".to_string(), json!("3")),
                ]),
            ),
            &mut context,
        )
        .expect("list tool");
    let payload: Value = serde_json::from_str(&list.content).expect("list payload");

    assert_eq!(payload["returned_count"], 2);
    assert_eq!(payload["max_results"], 2);
    assert_eq!(payload["scan_limit"], 3);
    assert_eq!(payload["count_is_estimate"], true);
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
fn local_workspace_backend_replaces_invalid_utf8_when_reading_text() {
    let workspace = tempfile::tempdir().expect("workspace");
    let path = workspace.path().join("mixed.log");
    std::fs::write(&path, b"ok\xffdone").expect("write invalid utf8");
    let local = LocalWorkspaceBackend::new(workspace.path());

    let text = local.read_text("mixed.log").expect("read text");

    assert_eq!(text, "ok\u{fffd}done");
}

#[cfg(unix)]
#[test]
fn local_workspace_backend_skips_unreadable_dirs_when_listing_files() {
    use std::os::unix::fs::PermissionsExt;

    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::write(workspace.path().join("visible.txt"), "visible").expect("visible file");
    let private_dir = workspace.path().join("private");
    std::fs::create_dir(&private_dir).expect("private dir");
    std::fs::write(private_dir.join("hidden.txt"), "hidden").expect("hidden file");
    std::fs::set_permissions(&private_dir, std::fs::Permissions::from_mode(0o000))
        .expect("make private dir unreadable");

    let local = LocalWorkspaceBackend::new(workspace.path());
    let listed = local.list_files(".", "**/*.txt");

    std::fs::set_permissions(&private_dir, std::fs::Permissions::from_mode(0o700))
        .expect("restore private dir permissions");

    assert_eq!(
        listed.expect("list should skip unreadable dir"),
        vec!["visible.txt"]
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
fn workspace_backends_honor_agent_glob_and_missing_file_semantics() {
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::create_dir_all(workspace.path().join("src/nested")).expect("dirs");
    std::fs::write(workspace.path().join("root.rs"), "fn root() {}").expect("root");
    std::fs::write(workspace.path().join("src/main.rs"), "fn main() {}").expect("main");
    std::fs::write(workspace.path().join("src/readme.md"), "# readme").expect("readme");
    std::fs::write(workspace.path().join("src/nested/lib.rs"), "fn lib() {}").expect("lib");
    let local = LocalWorkspaceBackend::new(workspace.path());

    assert_eq!(
        local.list_files(".", "**/*.rs").expect("local root glob"),
        vec![
            "root.rs".to_string(),
            "src/main.rs".to_string(),
            "src/nested/lib.rs".to_string(),
        ]
    );
    assert_eq!(
        local.list_files("src", "*.rs").expect("local base glob"),
        vec!["src/main.rs".to_string()]
    );

    let memory = MemoryWorkspaceBackend::default();
    memory.mkdir("src/nested").expect("mkdir");
    memory
        .write_text("/src/main.rs", "fn main() {}", false)
        .expect("write main");
    memory
        .write_text("src/nested/lib.rs", "fn lib() {}", false)
        .expect("write lib");
    memory
        .write_text("src/readme.md", "# readme", false)
        .expect("write readme");

    assert_eq!(
        memory.list_files(".", "**/*.rs").expect("memory root glob"),
        vec!["src/main.rs".to_string(), "src/nested/lib.rs".to_string()]
    );
    assert_eq!(
        memory.list_files("src", "*.rs").expect("memory base glob"),
        vec!["src/main.rs".to_string()]
    );
    assert_eq!(
        memory.read_text("missing.txt").expect_err("missing").kind(),
        ErrorKind::NotFound
    );
    let dir_info = memory.file_info("src").expect("dir info").expect("src dir");
    assert!(dir_info.is_dir);
    assert!(!dir_info.is_file);
    assert!(
        chrono::DateTime::parse_from_rfc3339(&dir_info.modified_at).is_ok(),
        "memory dir modified_at should match UTC ISO format, got {:?}",
        dir_info.modified_at
    );
    let file_info = memory
        .file_info("src/main.rs")
        .expect("file info")
        .expect("main file");
    assert!(
        chrono::DateTime::parse_from_rfc3339(&file_info.modified_at).is_ok(),
        "memory file modified_at should match UTC ISO format, got {:?}",
        file_info.modified_at
    );
    assert!(memory.exists("src"));
    assert!(!memory.is_file("src"));
}

#[test]
fn s3_workspace_backend_uses_prefix_and_object_store_semantics() {
    let backend = S3WorkspaceBackend::from_object_store(InMemory::new(), "tenant/workspace")
        .expect("s3 backend");

    backend
        .write_text("notes/a.txt", "one", false)
        .expect("write");
    backend
        .write_text("notes/a.txt", "+two", true)
        .expect("append");
    backend
        .write_text("src/main.rs", "fn main() {}", false)
        .expect("write rust");
    backend
        .write_text("src/readme.md", "# readme", false)
        .expect("write markdown");

    assert_eq!(backend.read_text("notes/a.txt").expect("read"), "one+two");
    assert_eq!(
        backend.read_bytes("notes/a.txt").expect("read bytes"),
        b"one+two"
    );
    assert_eq!(
        backend.list_files(".", "**/*.rs").expect("root glob"),
        vec!["src/main.rs".to_string()]
    );
    assert_eq!(
        backend.list_files("src", "*.rs").expect("base glob"),
        vec!["src/main.rs".to_string()]
    );

    let info = backend
        .file_info("notes/a.txt")
        .expect("file info")
        .expect("exists");
    assert_eq!(info.path, "notes/a.txt");
    assert!(info.is_file);
    assert!(!info.is_dir);
    assert_eq!(info.size, 7);
    assert_eq!(info.suffix, ".txt");
    assert!(!info.modified_at.is_empty());
    assert!(backend.exists("notes/a.txt"));
    assert!(backend.is_file("notes/a.txt"));
    assert!(!backend.exists("missing.txt"));
    assert_eq!(
        backend
            .read_text("missing.txt")
            .expect_err("missing")
            .kind(),
        ErrorKind::NotFound
    );

    backend.mkdir("empty/dir").expect("mkdir no-op");
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
