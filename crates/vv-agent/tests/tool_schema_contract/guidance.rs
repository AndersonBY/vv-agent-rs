use vv_agent::build_default_registry;

use super::helpers::{
    assert_nested_property_contains, assert_property_contains, description,
    nested_property_description, property_description,
};

#[test]
fn critical_tool_schemas_include_actionable_agent_guidance() {
    let registry = build_default_registry();

    let task_finish = description(&registry, "task_finish");
    assert!(task_finish.contains("Only call this when"));
    assert!(task_finish.contains("unfinished TODO"));
    assert!(task_finish.contains("runtime rejects premature finish by default"));

    let file_str_replace = description(&registry, "file_str_replace");
    assert!(file_str_replace.contains("exact `old_str`"));
    assert!(file_str_replace.contains("Call `read_file` first"));
    assert!(file_str_replace.contains("fails if `old_str` is not found"));

    let compress_memory = description(&registry, "compress_memory");
    assert!(compress_memory.contains("durable memory note"));
    assert!(compress_memory.contains("future compaction"));

    let file_info = description(&registry, "file_info");
    assert!(file_info.contains("Use before reading"));
    assert!(file_info.contains("large or binary"));

    let read_image = description(&registry, "read_image");
    assert!(read_image.contains("multimodal"));
    assert!(read_image.contains("Use this before reasoning about image content"));
}

#[test]
fn high_impact_tool_descriptions_use_operational_sections() {
    let registry = build_default_registry();

    for tool_name in [
        "read_file",
        "write_file",
        "list_files",
        "file_info",
        "workspace_grep",
        "file_str_replace",
        "bash",
        "check_background_command",
        "compress_memory",
        "todo_write",
        "create_sub_task",
        "sub_task_status",
        "read_image",
        "task_finish",
        "ask_user",
        "activate_skill",
    ] {
        let description = description(&registry, tool_name);
        assert!(
            description.len() >= 280,
            "{tool_name} description is too short to guide agent behavior: {description}"
        );
        assert!(
            description.contains("When to use:")
                || description.contains("Workflow:")
                || description.contains("Protocol:")
                || description.contains("Guidelines:")
                || description.contains("Modes:")
                || description.contains("Capabilities:"),
            "{tool_name} description lacks an operational section: {description}"
        );
    }

    let read_file = description(&registry, "read_file");
    assert!(read_file.contains("When to use:"));
    assert!(read_file.contains("Returns:"));
    assert!(read_file.contains("Safety and limits:"));

    let write_file = description(&registry, "write_file");
    assert!(write_file.contains("When to use:"));
    assert!(write_file.contains("Do not use this for surgical edits"));
    assert!(write_file.contains("Returns:"));

    let list_files = description(&registry, "list_files");
    assert!(list_files.contains("When to use:"));
    assert!(list_files.contains("Narrow first"));
    assert!(list_files.contains("Returns:"));

    let file_info = description(&registry, "file_info");
    assert!(file_info.contains("When to use:"));
    assert!(file_info.contains("before deciding read ranges"));
    assert!(file_info.contains("Returns:"));

    let file_str_replace = description(&registry, "file_str_replace");
    assert!(file_str_replace.contains("Workflow:"));
    assert!(file_str_replace.contains("never guess whitespace"));
    assert!(file_str_replace.contains("Returns:"));

    let check_background = description(&registry, "check_background_command");
    assert!(check_background.contains("When to use:"));
    assert!(check_background.contains("Polling protocol:"));
    assert!(check_background.contains("Returns:"));

    let compress_memory = description(&registry, "compress_memory");
    assert!(compress_memory.contains("When to use:"));
    assert!(compress_memory.contains("Do not store transient chatter"));
    assert!(compress_memory.contains("Good memory notes"));

    let read_image = description(&registry, "read_image");
    assert!(read_image.contains("When to use:"));
    assert!(read_image.contains("Supported inputs:"));
    assert!(read_image.contains("Returns:"));
}

#[test]
fn tool_descriptions_do_not_repeat_operational_guidance() {
    let registry = build_default_registry();

    for (tool_name, repeated_phrases) in [
        (
            "ask_user",
            vec!["Pause execution and ask the user for required clarification"],
        ),
        (
            "check_background_command",
            vec!["command launched in background mode"],
        ),
        (
            "read_image",
            vec![
                "attach it to the next LLM turn as multimodal content",
                "before reasoning about image content",
            ],
        ),
        ("file_info", vec!["before reading large or binary files"]),
        ("write_file", vec!["Parent directories may be created"]),
        (
            "workspace_grep",
            vec!["Prefer this tool over ad-hoc shell grep"],
        ),
    ] {
        let description = description(&registry, tool_name).to_lowercase();
        for phrase in repeated_phrases {
            let phrase = phrase.to_lowercase();
            let count = description.matches(&phrase).count();
            assert!(
                count <= 1,
                "{tool_name} repeats guidance phrase `{phrase}` {count} times:\n{description}"
            );
        }
    }
}

#[test]
fn every_builtin_tool_schema_has_operational_guidance_not_just_labels() {
    let registry = build_default_registry();

    let list_files = description(&registry, "list_files");
    assert!(list_files.contains("Use `path`"));
    assert!(list_files.contains("ignored_roots"));
    assert!(list_files.contains("truncated"));
    assert!(
        property_description(&registry, "list_files", "scan_limit").contains("count_is_estimate")
    );

    let write_file = description(&registry, "write_file");
    assert!(write_file.contains("Prefer `file_str_replace`"));
    assert!(write_file.contains("create parent directories"));
    assert!(write_file.contains("append=true"));

    let ask_user = description(&registry, "ask_user");
    assert!(ask_user.contains("Do not use this for facts"));
    assert!(ask_user.contains("blocks the runtime"));

    let check_background = description(&registry, "check_background_command");
    assert!(check_background.contains("running"));
    assert!(check_background.contains("completed"));
    assert!(check_background.contains("background_command_failed"));
    assert!(
        property_description(&registry, "check_background_command", "session_id")
            .contains("returned by `bash`")
    );

    let read_image = description(&registry, "read_image");
    assert!(read_image.contains("Supported formats"));
    assert!(read_image.contains("5 MiB"));
    assert!(read_image.contains("HTTP URLs are passed through"));
}

#[test]
fn high_impact_tool_parameters_include_operational_guidance() {
    let registry = build_default_registry();

    for (tool_name, property_name, required_terms) in [
        (
            "bash",
            "stdin",
            vec!["interactive", "confirmation", "heredoc"],
        ),
        (
            "bash",
            "auto_confirm",
            vec!["non-interactive", "yes", "destructive"],
        ),
        (
            "workspace_grep",
            "pattern",
            vec!["regex", "escape", "literal"],
        ),
        (
            "workspace_grep",
            "type",
            vec!["shortcut", "supported", "unknown"],
        ),
        (
            "file_str_replace",
            "new_str",
            vec!["replacement", "preserve", "line endings"],
        ),
        (
            "todo_write",
            "todos",
            vec!["complete", "replacement", "omit"],
        ),
        ("read_file", "start_line", vec!["1-based", "chunk", "large"]),
        (
            "read_file",
            "end_line",
            vec!["inclusive", "start_line", "large"],
        ),
        (
            "read_file",
            "show_line_numbers",
            vec!["quote", "precise", "edits"],
        ),
        (
            "write_file",
            "append",
            vec!["preserve", "existing", "overwrite"],
        ),
        (
            "write_file",
            "leading_newline",
            vec!["append", "separator", "existing"],
        ),
        (
            "write_file",
            "trailing_newline",
            vec!["append", "line boundary", "next append"],
        ),
        (
            "create_sub_task",
            "output_requirements",
            vec!["success criteria", "format", "deliverables"],
        ),
        (
            "create_sub_task",
            "include_main_summary",
            vec!["context", "parent", "independent"],
        ),
        (
            "create_sub_task",
            "exclude_files_pattern",
            vec!["shared context", "large", "irrelevant"],
        ),
        (
            "sub_task_status",
            "workspace_file_limit",
            vec!["snapshot", "files", "noise"],
        ),
        ("list_files", "glob", vec!["filter", "extensions", "narrow"]),
        (
            "list_files",
            "include_hidden",
            vec!["hidden", "dotfiles", "explicitly"],
        ),
        (
            "list_files",
            "include_ignored",
            vec!["dependency", "cache", "explicitly"],
        ),
        (
            "list_files",
            "max_results",
            vec!["returned", "follow-up", "narrow"],
        ),
        (
            "workspace_grep",
            "glob",
            vec!["filter", "extension", "narrow"],
        ),
        (
            "workspace_grep",
            "include_hidden",
            vec!["hidden", "dotfiles", "explicitly"],
        ),
        (
            "workspace_grep",
            "include_ignored",
            vec!["dependency", "cache", "explicitly"],
        ),
        (
            "workspace_grep",
            "head_limit",
            vec!["cap", "rows", "follow-up"],
        ),
        (
            "workspace_grep",
            "max_results",
            vec!["same behavior", "head_limit", "cap"],
        ),
        (
            "bash",
            "run_in_background",
            vec!["long-running", "session_id", "poll"],
        ),
        (
            "bash",
            "timeout",
            vec!["foreground", "background", "long-running"],
        ),
    ] {
        let description = property_description(&registry, tool_name, property_name);
        let normalized = description.to_lowercase();
        for term in required_terms {
            assert!(
                normalized.contains(term),
                "{tool_name}.{property_name} description should mention `{term}`: {description}"
            );
        }
    }

    for (property_path, required_terms) in [
        (
            vec!["todos", "items", "title"],
            vec!["actionable", "observable"],
        ),
        (
            vec!["todos", "items", "status"],
            vec!["pending", "in_progress", "completed"],
        ),
        (
            vec!["todos", "items", "priority"],
            vec!["high", "medium", "low"],
        ),
    ] {
        let description = nested_property_description(&registry, "todo_write", &property_path);
        let normalized = description.to_lowercase();
        for term in required_terms {
            assert!(
                normalized.contains(term),
                "todo_write.{} description should mention `{term}`: {description}",
                property_path.join(".")
            );
        }
    }
}

#[test]
fn tool_parameter_descriptions_are_operational_not_terse_labels() {
    let registry = build_default_registry();

    assert_property_contains(
        &registry,
        "task_finish",
        "exposed_files",
        &["workspace-relative", "created or modified", "deliverables"],
    );
    assert_property_contains(
        &registry,
        "create_sub_task",
        "agent_id",
        &["Exact", "configured `sub_agents`", "Do not pass"],
    );
    assert_property_contains(
        &registry,
        "create_sub_task",
        "task_description",
        &["self-contained", "concrete objective", "evidence"],
    );
    assert_property_contains(
        &registry,
        "write_file",
        "content",
        &[
            "complete file body",
            "append=true",
            "preserve existing content",
        ],
    );
    assert_property_contains(
        &registry,
        "workspace_grep",
        "output_mode",
        &["content", "files_with_matches", "count", "follow-up"],
    );
    assert_property_contains(
        &registry,
        "workspace_grep",
        "case_sensitive",
        &["smart-case", "literal casing"],
    );
    assert_property_contains(
        &registry,
        "file_str_replace",
        "replace_all",
        &["confirming every match", "Default false"],
    );
    assert_property_contains(
        &registry,
        "file_str_replace",
        "max_replacements",
        &["replace_all=false", "avoid accidental broad edits"],
    );
    assert_nested_property_contains(
        &registry,
        "create_sub_task",
        &["tasks", "items", "task_description"],
        &["independent", "concrete objective"],
    );
    assert_property_contains(
        &registry,
        "read_image",
        "path",
        &["PNG, JPEG, WEBP, or BMP", "HTTP URLs are passed through"],
    );
    assert_property_contains(
        &registry,
        "sub_task_status",
        "task_ids",
        &["returned by `create_sub_task`", "deduplicated", "first id"],
    );
}
