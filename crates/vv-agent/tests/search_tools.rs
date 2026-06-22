use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::json;
use vv_agent::workspace::{MemoryWorkspaceBackend, WorkspaceBackend};
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
fn workspace_grep_uses_regex_patterns() {
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
fn workspace_grep_coerces_scalar_pattern() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(workspace.path().join("numbers.txt"), "123\n456").expect("file");

    let result = registry
        .execute(
            &ToolCall::new(
                "grep_scalar_pattern",
                "workspace_grep",
                BTreeMap::from([("pattern".to_string(), json!(123))]),
            ),
            &mut context,
        )
        .expect("workspace_grep scalar pattern");

    assert_eq!(result.status, ToolResultStatus::Success);
    assert_eq!(result.metadata["summary"]["total_matches"], 1);
    assert_eq!(result.metadata["matches"][0]["text"], "123");
}

#[test]
fn workspace_grep_returns_agent_text_content_and_structured_metadata() {
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
fn workspace_grep_caps_structured_payload_without_duplication() {
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
fn workspace_grep_truncates_large_text_content() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(
        workspace.path().join("huge.txt"),
        format!("token {}\n", "x".repeat(40_000)),
    )
    .expect("file");

    let result = registry
        .execute(
            &ToolCall::new(
                "grep_text_truncation",
                "workspace_grep",
                BTreeMap::from([("pattern".to_string(), json!("token"))]),
            ),
            &mut context,
        )
        .expect("workspace_grep");

    assert_eq!(result.status, ToolResultStatus::Success);
    assert_eq!(result.metadata["summary"]["total_matches"], 1);
    assert_eq!(result.metadata["content_truncated"], true);
    assert_eq!(result.metadata["structured_truncated"], false);
    assert_eq!(result.metadata["truncated"], true);
    assert!(result.content.contains("--- TRUNCATED ---"));
    assert!(result
        .content
        .contains("Use a narrower pattern/path/glob/type/head_limit"));
    assert!(result.content.len() < 35_000);
}

#[test]
fn workspace_grep_reports_supported_types_for_unknown_type() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());

    let result = registry
        .execute(
            &ToolCall::new(
                "grep_unknown_type",
                "workspace_grep",
                BTreeMap::from([
                    ("pattern".to_string(), json!("token")),
                    ("type".to_string(), json!("unknown")),
                ]),
            ),
            &mut context,
        )
        .expect("workspace_grep");

    assert_eq!(result.status, ToolResultStatus::Error);
    assert!(result
        .content
        .contains("Unsupported file type: unknown. Supported types:"));
    assert_eq!(result.metadata["error"], result.content);
    assert!(result.content.contains("dockerfile"));
    assert!(result.content.contains("makefile"));
}

#[test]
fn workspace_grep_reports_scalar_type_as_unsupported() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());

    let result = registry
        .execute(
            &ToolCall::new(
                "grep_scalar_type",
                "workspace_grep",
                BTreeMap::from([
                    ("pattern".to_string(), json!("token")),
                    ("type".to_string(), json!(123)),
                ]),
            ),
            &mut context,
        )
        .expect("workspace_grep");

    assert_eq!(result.status, ToolResultStatus::Error);
    assert!(result
        .content
        .contains("Unsupported file type: 123. Supported types:"));
    assert_eq!(result.metadata["error"], result.content);
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
fn workspace_grep_applies_glob_relative_to_search_path() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::create_dir_all(workspace.path().join("src")).expect("dir");
    std::fs::write(workspace.path().join("src/main.rs"), "token rust").expect("rs");
    std::fs::write(workspace.path().join("src/readme.md"), "token docs").expect("md");

    let result = registry
        .execute(
            &ToolCall::new(
                "grep_glob",
                "workspace_grep",
                BTreeMap::from([
                    ("pattern".to_string(), json!("token")),
                    ("path".to_string(), json!("src")),
                    ("glob".to_string(), json!("*.rs")),
                ]),
            ),
            &mut context,
        )
        .expect("workspace_grep glob");

    assert_eq!(result.status, ToolResultStatus::Success);
    assert_eq!(result.metadata["summary"]["total_matches"], 1);
    let rows = result.metadata["matches"].as_array().expect("matches");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["path"], "src/main.rs");
}

#[test]
fn workspace_grep_uses_configured_workspace_backend() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let backend = MemoryWorkspaceBackend::default();
    backend.mkdir("src").expect("mkdir");
    backend
        .write_text("src/lib.rs", "token memory", false)
        .expect("write");
    backend
        .write_text("src/readme.md", "token docs", false)
        .expect("write");
    let mut context = ToolContext::new(workspace.path());
    context.workspace_backend = Arc::new(backend);

    let result = registry
        .execute(
            &ToolCall::new(
                "grep_memory_backend",
                "workspace_grep",
                BTreeMap::from([
                    ("pattern".to_string(), json!("token")),
                    ("path".to_string(), json!("src")),
                    ("glob".to_string(), json!("*.rs")),
                ]),
            ),
            &mut context,
        )
        .expect("workspace_grep memory backend");

    assert_eq!(result.status, ToolResultStatus::Success);
    assert_eq!(result.metadata["summary"]["files_searched"], 1);
    assert_eq!(result.metadata["summary"]["total_matches"], 1);
    assert_eq!(result.metadata["matches"][0]["path"], "src/lib.rs");
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
fn workspace_grep_uses_json_truthiness_for_hidden_and_line_flags() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(workspace.path().join(".hidden.txt"), "Agent hidden").expect("hidden");

    let hidden = registry
        .execute(
            &ToolCall::new(
                "grep_truthy_hidden",
                "workspace_grep",
                BTreeMap::from([
                    ("pattern".to_string(), json!("Agent")),
                    ("include_hidden".to_string(), json!("false")),
                ]),
            ),
            &mut context,
        )
        .expect("workspace_grep hidden");

    assert_eq!(hidden.metadata["summary"]["total_matches"], 1);

    let no_line_numbers = registry
        .execute(
            &ToolCall::new(
                "grep_falsey_line_numbers",
                "workspace_grep",
                BTreeMap::from([
                    ("pattern".to_string(), json!("Agent")),
                    ("path".to_string(), json!(".hidden.txt")),
                    ("n".to_string(), json!("")),
                ]),
            ),
            &mut context,
        )
        .expect("workspace_grep line numbers");

    assert!(no_line_numbers.metadata["matches"][0].get("line").is_none());
}

#[test]
fn workspace_grep_file_path_target_can_read_hidden_file() {
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
fn workspace_grep_file_path_target_can_read_inside_ignored_root() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::create_dir_all(workspace.path().join("node_modules/pkg")).expect("dir");
    std::fs::write(
        workspace.path().join("node_modules/pkg/x.js"),
        "const token = 'Agent';",
    )
    .expect("file");

    let result = registry
        .execute(
            &ToolCall::new(
                "grep_file_ignored_root_target",
                "workspace_grep",
                BTreeMap::from([
                    ("pattern".to_string(), json!("Agent")),
                    ("path".to_string(), json!("node_modules/pkg/x.js")),
                    ("output_mode".to_string(), json!("files_with_matches")),
                ]),
            ),
            &mut context,
        )
        .expect("workspace_grep ignored-root file target");

    assert_eq!(result.status, ToolResultStatus::Success);
    assert_eq!(result.metadata["files"], json!(["node_modules/pkg/x.js"]));
    assert_eq!(result.metadata["summary"]["files_searched"], 1);
    assert_eq!(result.metadata["summary"]["total_matches"], 1);
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
fn workspace_grep_accepts_string_limits_and_context() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(
        workspace.path().join("ctx.txt"),
        "before\nhit\nhit again\nafter",
    )
    .expect("file");

    let result = registry
        .execute(
            &ToolCall::new(
                "grep_string_limits",
                "workspace_grep",
                BTreeMap::from([
                    ("pattern".to_string(), json!("hit")),
                    ("c".to_string(), json!("1")),
                    ("head_limit".to_string(), json!("2")),
                ]),
            ),
            &mut context,
        )
        .expect("workspace_grep string limits");

    assert_eq!(result.metadata["head_limit"], 2);
    assert_eq!(result.metadata["head_limited"], true);
    let rows = result.metadata["matches"].as_array().expect("matches");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["line"], 1);
    assert_eq!(rows[1]["line"], 2);
}

#[test]
fn workspace_grep_ignores_removed_max_results_alias() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(workspace.path().join("hits.txt"), "hit one\nhit two").expect("file");

    let result = registry
        .execute(
            &ToolCall::new(
                "grep_removed_max_results",
                "workspace_grep",
                BTreeMap::from([
                    ("pattern".to_string(), json!("hit")),
                    ("output_mode".to_string(), json!("content")),
                    ("max_results".to_string(), json!(1)),
                ]),
            ),
            &mut context,
        )
        .expect("workspace_grep removed max_results");

    let rows = result.metadata["matches"].as_array().expect("matches");
    assert_eq!(rows.len(), 2);
    assert_ne!(result.metadata["head_limit"], json!(1));
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
