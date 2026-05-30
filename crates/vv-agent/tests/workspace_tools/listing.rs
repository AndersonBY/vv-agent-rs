use super::*;

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
