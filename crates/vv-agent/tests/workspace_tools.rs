use std::collections::BTreeMap;
use std::io::ErrorKind;

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
fn list_files_reports_estimated_count_when_scan_limit_is_reached_like_python() {
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

#[test]
fn workspace_backends_honor_python_glob_and_missing_file_semantics() {
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
