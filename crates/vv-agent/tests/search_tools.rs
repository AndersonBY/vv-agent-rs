use std::collections::BTreeMap;

use serde_json::json;
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
    assert_eq!(result.metadata["summary"]["total_matches"], 2);
    assert_eq!(result.metadata["matches"][0]["text"], "update lower");
    assert_eq!(result.metadata["matches"][1]["text"], "Update upper");
}

#[test]
fn workspace_grep_uses_regex_patterns_like_python() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(
        workspace.path().join("events.log"),
        "error 1\nwarning 2\nerror 203\n",
    )
    .expect("file");

    let result = registry
        .execute(
            &ToolCall::new(
                "grep_regex",
                "workspace_grep",
                BTreeMap::from([("pattern".to_string(), json!(r"error \d+"))]),
            ),
            &mut context,
        )
        .expect("workspace_grep regex");

    assert_eq!(result.metadata["summary"]["total_matches"], 2);
    let rows = result.metadata["matches"].as_array().expect("matches");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["line"], 1);
    assert_eq!(rows[1]["line"], 3);
}

#[test]
fn workspace_grep_returns_python_style_text_content_and_structured_metadata() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(workspace.path().join("match.txt"), "Agent line").expect("file");

    let result = registry
        .execute(
            &ToolCall::new(
                "grep_text_content",
                "workspace_grep",
                BTreeMap::from([("pattern".to_string(), json!("Agent"))]),
            ),
            &mut context,
        )
        .expect("workspace_grep");

    assert!(result.content.contains("Found 1 matches in 1 files"));
    assert!(!result.content.contains("\"matches\""));
    assert_eq!(result.metadata["summary"]["total_matches"], 1);
    assert_eq!(result.metadata["matches"][0]["path"], "match.txt");
}

#[test]
fn workspace_grep_caps_structured_payload_without_duplication_like_python() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    for index in 0..205 {
        std::fs::write(
            workspace.path().join(format!("match_{index:03}.txt")),
            "token\n",
        )
        .expect("file");
    }

    let result = registry
        .execute(
            &ToolCall::new(
                "grep_structured_cap",
                "workspace_grep",
                BTreeMap::from([
                    ("pattern".to_string(), json!("token")),
                    ("output_mode".to_string(), json!("content")),
                ]),
            ),
            &mut context,
        )
        .expect("workspace_grep");

    assert_eq!(result.metadata["total_result_items"], 205);
    assert_eq!(result.metadata["returned_count"], 200);
    assert_eq!(result.metadata["structured_truncated"], true);
    assert_eq!(result.metadata["truncated"], true);
    assert_eq!(result.metadata["structured_item_limit"], 200);
    assert_eq!(result.metadata["structured_char_limit"], 20_000);
    assert_eq!(
        result.metadata["matches"]
            .as_array()
            .expect("matches")
            .len(),
        200
    );
    assert!(result.content.contains("Showing first 200 rows."));
    assert!(!result.content.contains("\"matches\""));
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
    assert_eq!(files.metadata["files"], json!(["a.py", "b.py"]));
    assert_eq!(files.metadata["summary"]["total_matches"], 2);

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
    assert_eq!(count.metadata["file_counts"]["a.py"], 1);
    assert_eq!(count.metadata["file_counts"]["b.py"], 1);
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
    assert_eq!(default.metadata["summary"]["total_matches"], 0);

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
    assert_eq!(included.metadata["summary"]["total_matches"], 2);
}

#[test]
fn workspace_grep_file_path_target_can_read_hidden_file_like_python() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(workspace.path().join(".hidden.txt"), "secret Agent marker").expect("file");

    let result = registry
        .execute(
            &ToolCall::new(
                "grep_hidden_file_target",
                "workspace_grep",
                BTreeMap::from([
                    ("pattern".to_string(), json!("Agent")),
                    ("path".to_string(), json!(".hidden.txt")),
                ]),
            ),
            &mut context,
        )
        .expect("workspace_grep hidden file target");

    assert_eq!(result.metadata["summary"]["files_searched"], 1);
    assert_eq!(result.metadata["summary"]["total_matches"], 1);
    assert_eq!(result.metadata["matches"][0]["path"], ".hidden.txt");
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

    assert_eq!(result.metadata["summary"]["files_searched"], 1);
    assert_eq!(result.metadata["summary"]["total_matches"], 1);
    let lines = result.metadata["matches"].as_array().expect("matches");
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
