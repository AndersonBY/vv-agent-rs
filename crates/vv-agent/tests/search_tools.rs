use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::json;
use vv_agent::workspace::{MemoryWorkspaceBackend, WorkspaceBackend};
use vv_agent::{build_default_registry, ToolCall, ToolContext, ToolResultStatus};

#[cfg(unix)]
#[path = "search_tools/sensitive_fast_path.rs"]
mod sensitive_fast_path;

#[test]
fn old_search_tool_names_are_not_registered() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());

    let old_grep = registry.execute(
        &ToolCall::new(
            "old_grep",
            "workspace_grep",
            BTreeMap::from([("pattern".to_string(), json!("token"))]),
        ),
        &mut context,
    );
    assert!(old_grep.is_err());

    let old_list = registry.execute(
        &ToolCall::new("old_list", "list_files", BTreeMap::new()),
        &mut context,
    );
    assert!(old_list.is_err());
}

#[test]
fn search_files_defaults_to_files_with_matches() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(workspace.path().join("a.txt"), "token one").expect("a");
    std::fs::write(workspace.path().join("b.txt"), "token two").expect("b");

    let result = registry
        .execute(
            &ToolCall::new(
                "search_default",
                "search_files",
                BTreeMap::from([("pattern".to_string(), json!("token"))]),
            ),
            &mut context,
        )
        .expect("search_files");

    assert_eq!(result.status, ToolResultStatus::Success);
    assert_eq!(result.metadata["output_mode"], "files_with_matches");
    assert_eq!(result.metadata["files"], json!(["a.txt", "b.txt"]));
    assert!(!result.metadata.contains_key("matches"));
}

#[test]
fn search_files_literal_offset_and_unlimited_head_limit() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    for index in 0..4 {
        std::fs::write(
            workspace.path().join(format!("file_{index}.txt")),
            "a.b token",
        )
        .expect("file");
    }

    let paged = registry
        .execute(
            &ToolCall::new(
                "search_literal_page",
                "search_files",
                BTreeMap::from([
                    ("pattern".to_string(), json!("a.b")),
                    ("literal".to_string(), json!(true)),
                    ("offset".to_string(), json!(1)),
                    ("head_limit".to_string(), json!(2)),
                ]),
            ),
            &mut context,
        )
        .expect("search literal page");
    assert_eq!(paged.metadata["files"], json!(["file_1.txt", "file_2.txt"]));
    assert_eq!(paged.metadata["offset"], 1);
    assert_eq!(paged.metadata["head_limit"], 2);
    assert_eq!(paged.metadata["total_result_items"], 4);
    assert_eq!(paged.metadata["returned_count"], 2);

    let unlimited = registry
        .execute(
            &ToolCall::new(
                "search_literal_unlimited",
                "search_files",
                BTreeMap::from([
                    ("pattern".to_string(), json!("a.b")),
                    ("literal".to_string(), json!(true)),
                    ("head_limit".to_string(), json!(0)),
                ]),
            ),
            &mut context,
        )
        .expect("search literal unlimited");
    assert_eq!(unlimited.metadata["returned_count"], 4);
}

#[test]
fn search_files_omits_sensitive_paths_by_default() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(workspace.path().join(".env"), "TOKEN=secret").expect("env");
    std::fs::write(workspace.path().join("visible.txt"), "TOKEN=public").expect("visible");

    let default_result = registry
        .execute(
            &ToolCall::new(
                "search_sensitive_default",
                "search_files",
                BTreeMap::from([("pattern".to_string(), json!("TOKEN"))]),
            ),
            &mut context,
        )
        .expect("default sensitive search");
    assert_eq!(default_result.metadata["files"], json!(["visible.txt"]));
    assert_eq!(default_result.metadata["sensitive_files_omitted"], 1);

    let included = registry
        .execute(
            &ToolCall::new(
                "search_sensitive_included",
                "search_files",
                BTreeMap::from([
                    ("pattern".to_string(), json!("TOKEN")),
                    ("include_hidden".to_string(), json!(true)),
                    ("include_sensitive".to_string(), json!(true)),
                ]),
            ),
            &mut context,
        )
        .expect("included sensitive search");
    assert_eq!(included.metadata["files"], json!([".env", "visible.txt"]));
}

#[test]
fn search_files_finds_content_with_smart_case() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(workspace.path().join("a.txt"), "update lower\nUpdate upper").expect("file");

    let result = registry
        .execute(
            &ToolCall::new(
                "grep_1",
                "search_files",
                BTreeMap::from([
                    ("pattern".to_string(), json!("update")),
                    ("output_mode".to_string(), json!("content")),
                ]),
            ),
            &mut context,
        )
        .expect("search_files");

    assert_eq!(result.status, ToolResultStatus::Success);
    assert_eq!(result.metadata["summary"]["total_matches"], 2);
    assert_eq!(result.metadata["matches"][0]["text"], "update lower");
    assert_eq!(result.metadata["matches"][1]["text"], "Update upper");
}

#[test]
fn search_files_uses_regex_patterns() {
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
                "search_files",
                BTreeMap::from([
                    ("pattern".to_string(), json!(r"error \d+")),
                    ("output_mode".to_string(), json!("content")),
                ]),
            ),
            &mut context,
        )
        .expect("search_files regex");

    assert_eq!(result.metadata["summary"]["total_matches"], 2);
    let rows = result.metadata["matches"].as_array().expect("matches");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["line"], 1);
    assert_eq!(rows[1]["line"], 3);
}

#[test]
fn search_files_coerces_scalar_pattern() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(workspace.path().join("numbers.txt"), "123\n456").expect("file");

    let result = registry
        .execute(
            &ToolCall::new(
                "grep_scalar_pattern",
                "search_files",
                BTreeMap::from([
                    ("pattern".to_string(), json!(123)),
                    ("output_mode".to_string(), json!("content")),
                ]),
            ),
            &mut context,
        )
        .expect("search_files scalar pattern");

    assert_eq!(result.status, ToolResultStatus::Success);
    assert_eq!(result.metadata["summary"]["total_matches"], 1);
    assert_eq!(result.metadata["matches"][0]["text"], "123");
}

#[test]
fn search_files_returns_agent_text_content_and_structured_metadata() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(workspace.path().join("match.txt"), "Agent line").expect("file");

    let result = registry
        .execute(
            &ToolCall::new(
                "grep_text_content",
                "search_files",
                BTreeMap::from([
                    ("pattern".to_string(), json!("Agent")),
                    ("output_mode".to_string(), json!("content")),
                ]),
            ),
            &mut context,
        )
        .expect("search_files");

    assert!(result.content.contains("Found 1 matches in 1 files"));
    assert!(!result.content.contains("\"matches\""));
    assert_eq!(result.metadata["summary"]["total_matches"], 1);
    assert_eq!(result.metadata["matches"][0]["path"], "match.txt");
}

#[test]
fn search_files_caps_structured_payload_without_duplication() {
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
                "search_files",
                BTreeMap::from([
                    ("pattern".to_string(), json!("token")),
                    ("output_mode".to_string(), json!("content")),
                ]),
            ),
            &mut context,
        )
        .expect("search_files");

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
fn search_files_truncates_large_text_content() {
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
                "search_files",
                BTreeMap::from([
                    ("pattern".to_string(), json!("token")),
                    ("output_mode".to_string(), json!("content")),
                ]),
            ),
            &mut context,
        )
        .expect("search_files");

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
fn search_files_reports_supported_types_for_unknown_type() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());

    let result = registry
        .execute(
            &ToolCall::new(
                "grep_unknown_type",
                "search_files",
                BTreeMap::from([
                    ("pattern".to_string(), json!("token")),
                    ("type".to_string(), json!("unknown")),
                ]),
            ),
            &mut context,
        )
        .expect("search_files");

    assert_eq!(result.status, ToolResultStatus::Error);
    assert!(result
        .content
        .contains("Unsupported file type: unknown. Supported types:"));
    assert_eq!(result.metadata["error"], result.content);
    assert!(result.content.contains("dockerfile"));
    assert!(result.content.contains("makefile"));
}

#[test]
fn search_files_reports_scalar_type_as_unsupported() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());

    let result = registry
        .execute(
            &ToolCall::new(
                "grep_scalar_type",
                "search_files",
                BTreeMap::from([
                    ("pattern".to_string(), json!("token")),
                    ("type".to_string(), json!(123)),
                ]),
            ),
            &mut context,
        )
        .expect("search_files");

    assert_eq!(result.status, ToolResultStatus::Error);
    assert!(result
        .content
        .contains("Unsupported file type: 123. Supported types:"));
    assert_eq!(result.metadata["error"], result.content);
}

#[test]
fn search_files_supports_files_and_count_modes_with_type_filter() {
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
                "search_files",
                BTreeMap::from([
                    ("pattern".to_string(), json!("token")),
                    ("output_mode".to_string(), json!("files_with_matches")),
                    ("type".to_string(), json!("py")),
                ]),
            ),
            &mut context,
        )
        .expect("search_files files");
    assert_eq!(files.metadata["files"], json!(["a.py", "b.py"]));
    assert_eq!(files.metadata["summary"]["total_matches"], 2);

    let count = registry
        .execute(
            &ToolCall::new(
                "grep_count",
                "search_files",
                BTreeMap::from([
                    ("pattern".to_string(), json!("token")),
                    ("output_mode".to_string(), json!("count")),
                    ("type".to_string(), json!("py")),
                ]),
            ),
            &mut context,
        )
        .expect("search_files count");
    assert_eq!(count.metadata["file_counts"]["a.py"], 1);
    assert_eq!(count.metadata["file_counts"]["b.py"], 1);
}

#[test]
fn search_files_applies_glob_relative_to_search_path() {
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
                "search_files",
                BTreeMap::from([
                    ("pattern".to_string(), json!("token")),
                    ("path".to_string(), json!("src")),
                    ("glob".to_string(), json!("*.rs")),
                    ("output_mode".to_string(), json!("content")),
                ]),
            ),
            &mut context,
        )
        .expect("search_files glob");

    assert_eq!(result.status, ToolResultStatus::Success);
    assert_eq!(result.metadata["summary"]["total_matches"], 1);
    let rows = result.metadata["matches"].as_array().expect("matches");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["path"], "src/main.rs");
}

#[test]
fn search_files_uses_configured_workspace_backend() {
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
                "search_files",
                BTreeMap::from([
                    ("pattern".to_string(), json!("token")),
                    ("path".to_string(), json!("src")),
                    ("glob".to_string(), json!("*.rs")),
                    ("output_mode".to_string(), json!("content")),
                ]),
            ),
            &mut context,
        )
        .expect("search_files memory backend");

    assert_eq!(result.status, ToolResultStatus::Success);
    assert_eq!(result.metadata["summary"]["files_searched"], 1);
    assert_eq!(result.metadata["summary"]["total_matches"], 1);
    assert_eq!(result.metadata["matches"][0]["path"], "src/lib.rs");
}

#[test]
fn search_files_respects_hidden_and_ignored_defaults() {
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
                "search_files",
                BTreeMap::from([("pattern".to_string(), json!("Agent"))]),
            ),
            &mut context,
        )
        .expect("search_files default");
    assert_eq!(default.metadata["summary"]["total_matches"], 0);

    let included = registry
        .execute(
            &ToolCall::new(
                "grep_included",
                "search_files",
                BTreeMap::from([
                    ("pattern".to_string(), json!("Agent")),
                    ("include_hidden".to_string(), json!(true)),
                    ("include_ignored".to_string(), json!(true)),
                ]),
            ),
            &mut context,
        )
        .expect("search_files included");
    assert_eq!(included.metadata["summary"]["total_matches"], 2);
}

#[test]
fn search_files_uses_json_truthiness_for_hidden_and_line_flags() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(workspace.path().join(".hidden.txt"), "Agent hidden").expect("hidden");

    let hidden = registry
        .execute(
            &ToolCall::new(
                "grep_truthy_hidden",
                "search_files",
                BTreeMap::from([
                    ("pattern".to_string(), json!("Agent")),
                    ("include_hidden".to_string(), json!("false")),
                ]),
            ),
            &mut context,
        )
        .expect("search_files hidden");

    assert_eq!(hidden.metadata["summary"]["total_matches"], 1);

    let no_line_numbers = registry
        .execute(
            &ToolCall::new(
                "grep_falsey_line_numbers",
                "search_files",
                BTreeMap::from([
                    ("pattern".to_string(), json!("Agent")),
                    ("path".to_string(), json!(".hidden.txt")),
                    ("output_mode".to_string(), json!("content")),
                    ("n".to_string(), json!("")),
                ]),
            ),
            &mut context,
        )
        .expect("search_files line numbers");

    assert!(no_line_numbers.metadata["matches"][0].get("line").is_none());
}

#[test]
fn search_files_file_path_target_can_read_hidden_file() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(workspace.path().join(".hidden.txt"), "secret Agent marker").expect("file");

    let result = registry
        .execute(
            &ToolCall::new(
                "grep_hidden_file_target",
                "search_files",
                BTreeMap::from([
                    ("pattern".to_string(), json!("Agent")),
                    ("path".to_string(), json!(".hidden.txt")),
                    ("output_mode".to_string(), json!("content")),
                ]),
            ),
            &mut context,
        )
        .expect("search_files hidden file target");

    assert_eq!(result.metadata["summary"]["files_searched"], 1);
    assert_eq!(result.metadata["summary"]["total_matches"], 1);
    assert_eq!(result.metadata["matches"][0]["path"], ".hidden.txt");
}

#[test]
fn search_files_file_path_target_can_read_inside_ignored_root() {
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
                "search_files",
                BTreeMap::from([
                    ("pattern".to_string(), json!("Agent")),
                    ("path".to_string(), json!("node_modules/pkg/x.js")),
                    ("output_mode".to_string(), json!("files_with_matches")),
                ]),
            ),
            &mut context,
        )
        .expect("search_files ignored-root file target");

    assert_eq!(result.status, ToolResultStatus::Success);
    assert_eq!(result.metadata["files"], json!(["node_modules/pkg/x.js"]));
    assert_eq!(result.metadata["summary"]["files_searched"], 1);
    assert_eq!(result.metadata["summary"]["total_matches"], 1);
}

#[test]
fn search_files_supports_context_lines_and_file_targets() {
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
                "search_files",
                BTreeMap::from([
                    ("pattern".to_string(), json!("Agent")),
                    ("path".to_string(), json!("articles/essay.md")),
                    ("output_mode".to_string(), json!("content")),
                    ("c".to_string(), json!(1)),
                ]),
            ),
            &mut context,
        )
        .expect("search_files context");

    assert_eq!(result.metadata["summary"]["files_searched"], 1);
    assert_eq!(result.metadata["summary"]["total_matches"], 1);
    let lines = result.metadata["matches"].as_array().expect("matches");
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0]["is_match"], false);
    assert_eq!(lines[1]["is_match"], true);
}

#[test]
fn search_files_accepts_string_limits_and_context() {
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
                "search_files",
                BTreeMap::from([
                    ("pattern".to_string(), json!("hit")),
                    ("output_mode".to_string(), json!("content")),
                    ("c".to_string(), json!("1")),
                    ("head_limit".to_string(), json!("2")),
                ]),
            ),
            &mut context,
        )
        .expect("search_files string limits");

    assert_eq!(result.metadata["head_limit"], 2);
    assert_eq!(result.metadata["head_limited"], true);
    let rows = result.metadata["matches"].as_array().expect("matches");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["line"], 1);
    assert_eq!(rows[1]["line"], 2);
}

#[test]
fn search_files_ignores_removed_max_results_alias() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    std::fs::write(workspace.path().join("hits.txt"), "hit one\nhit two").expect("file");

    let result = registry
        .execute(
            &ToolCall::new(
                "grep_removed_max_results",
                "search_files",
                BTreeMap::from([
                    ("pattern".to_string(), json!("hit")),
                    ("output_mode".to_string(), json!("content")),
                    ("max_results".to_string(), json!(1)),
                ]),
            ),
            &mut context,
        )
        .expect("search_files removed max_results");

    let rows = result.metadata["matches"].as_array().expect("matches");
    assert_eq!(rows.len(), 2);
    assert_ne!(result.metadata["head_limit"], json!(1));
}

#[test]
fn search_files_rejects_paths_outside_workspace_by_default() {
    let workspace = tempfile::tempdir().expect("workspace");
    let outside = tempfile::tempdir().expect("outside");
    std::fs::write(outside.path().join("secret.txt"), "Agent outside").expect("outside file");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());

    let result = registry
        .execute(
            &ToolCall::new(
                "grep_escape",
                "search_files",
                BTreeMap::from([
                    ("pattern".to_string(), json!("Agent")),
                    ("path".to_string(), json!(outside.path())),
                ]),
            ),
            &mut context,
        )
        .expect("search_files");

    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(result.error_code.as_deref(), Some("path_escapes_workspace"));
}
