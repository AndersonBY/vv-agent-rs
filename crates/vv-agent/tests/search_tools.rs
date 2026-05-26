use std::collections::BTreeMap;

use serde_json::{json, Value};
use vv_agent::{build_default_registry, ToolCall, ToolContext, ToolResultStatus};

#[test]
fn workspace_grep_finds_content_with_smart_case() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(workspace.path().join("a.txt"), "update lower\nUpdate upper").expect("file");

    let result = registry
        .execute(
            &ToolCall::new(
                "grep_1",
                "workspace_grep",
                BTreeMap::from([("pattern".to_string(), json!("update"))]),
            ),
            &mut context,
        )
        .expect("workspace_grep");

    assert_eq!(result.status, ToolResultStatus::Success);
    let payload: Value = serde_json::from_str(&result.content).expect("payload");
    assert_eq!(payload["summary"]["total_matches"], 2);
    assert_eq!(payload["matches"][0]["text"], "update lower");
    assert_eq!(payload["matches"][1]["text"], "Update upper");
}

#[test]
fn workspace_grep_supports_files_and_count_modes_with_type_filter() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(workspace.path().join("a.py"), "TOKEN = 1").expect("a");
    std::fs::write(workspace.path().join("b.py"), "token = 2").expect("b");
    std::fs::write(workspace.path().join("c.md"), "token = 3").expect("c");

    let files = registry
        .execute(
            &ToolCall::new(
                "grep_files",
                "workspace_grep",
                BTreeMap::from([
                    ("pattern".to_string(), json!("token")),
                    ("output_mode".to_string(), json!("files_with_matches")),
                    ("type".to_string(), json!("py")),
                ]),
            ),
            &mut context,
        )
        .expect("workspace_grep files");
    let files_payload: Value = serde_json::from_str(&files.content).expect("files payload");
    assert_eq!(files_payload["files"], json!(["a.py", "b.py"]));
    assert_eq!(files_payload["summary"]["total_matches"], 2);

    let count = registry
        .execute(
            &ToolCall::new(
                "grep_count",
                "workspace_grep",
                BTreeMap::from([
                    ("pattern".to_string(), json!("token")),
                    ("output_mode".to_string(), json!("count")),
                    ("type".to_string(), json!("py")),
                ]),
            ),
            &mut context,
        )
        .expect("workspace_grep count");
    let count_payload: Value = serde_json::from_str(&count.content).expect("count payload");
    assert_eq!(count_payload["file_counts"]["a.py"], 1);
    assert_eq!(count_payload["file_counts"]["b.py"], 1);
}

#[test]
fn workspace_grep_respects_hidden_and_ignored_defaults() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(workspace.path().join(".hidden.txt"), "Agent hidden").expect("hidden");
    std::fs::create_dir_all(workspace.path().join("node_modules/pkg")).expect("dir");
    std::fs::write(
        workspace.path().join("node_modules/pkg/x.js"),
        "Agent ignored",
    )
    .expect("ignored");

    let default = registry
        .execute(
            &ToolCall::new(
                "grep_default",
                "workspace_grep",
                BTreeMap::from([("pattern".to_string(), json!("Agent"))]),
            ),
            &mut context,
        )
        .expect("workspace_grep default");
    let default_payload: Value = serde_json::from_str(&default.content).expect("default payload");
    assert_eq!(default_payload["summary"]["total_matches"], 0);

    let included = registry
        .execute(
            &ToolCall::new(
                "grep_included",
                "workspace_grep",
                BTreeMap::from([
                    ("pattern".to_string(), json!("Agent")),
                    ("include_hidden".to_string(), json!(true)),
                    ("include_ignored".to_string(), json!(true)),
                ]),
            ),
            &mut context,
        )
        .expect("workspace_grep included");
    let included_payload: Value =
        serde_json::from_str(&included.content).expect("included payload");
    assert_eq!(included_payload["summary"]["total_matches"], 2);
}

#[test]
fn workspace_grep_supports_context_lines_and_file_targets() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::create_dir_all(workspace.path().join("articles")).expect("dir");
    std::fs::write(
        workspace.path().join("articles/essay.md"),
        "intro\nabout Agent design\noutro",
    )
    .expect("file");

    let result = registry
        .execute(
            &ToolCall::new(
                "grep_context",
                "workspace_grep",
                BTreeMap::from([
                    ("pattern".to_string(), json!("Agent")),
                    ("path".to_string(), json!("articles/essay.md")),
                    ("c".to_string(), json!(1)),
                ]),
            ),
            &mut context,
        )
        .expect("workspace_grep context");

    let payload: Value = serde_json::from_str(&result.content).expect("payload");
    assert_eq!(payload["summary"]["files_searched"], 1);
    assert_eq!(payload["summary"]["total_matches"], 1);
    let lines = payload["matches"].as_array().expect("matches");
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0]["is_match"], false);
    assert_eq!(lines[1]["is_match"], true);
}

#[test]
fn workspace_grep_rejects_paths_outside_workspace_by_default() {
    let workspace = tempfile::tempdir().expect("workspace");
    let outside = tempfile::tempdir().expect("outside");
    std::fs::write(outside.path().join("secret.txt"), "Agent outside").expect("outside file");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());

    let result = registry
        .execute(
            &ToolCall::new(
                "grep_escape",
                "workspace_grep",
                BTreeMap::from([
                    ("pattern".to_string(), json!("Agent")),
                    ("path".to_string(), json!(outside.path())),
                ]),
            ),
            &mut context,
        )
        .expect("workspace_grep");

    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(result.error_code.as_deref(), Some("path_escapes_workspace"));
}
