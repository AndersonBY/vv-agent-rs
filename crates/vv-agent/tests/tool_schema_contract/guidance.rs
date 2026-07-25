use vv_agent::build_default_registry;

use super::helpers::{description, property_description};

#[test]
fn concise_tool_descriptions_keep_only_runtime_relevant_guidance() {
    let registry = build_default_registry();

    let task_finish = description(&registry, "task_finish");
    assert!(task_finish.contains("optional"));
    assert!(task_finish.contains("no-tool policy"));
    assert!(task_finish.contains("unfinished-TODO checks"));

    let edit_file = description(&registry, "edit_file");
    assert!(edit_file.contains("current read/write baseline"));
    assert!(edit_file.contains("stale baselines are rejected"));

    let file_info = description(&registry, "file_info");
    assert!(file_info.contains("without reading file contents"));

    let read_image = description(&registry, "read_image");
    assert!(read_image.contains("next model turn"));
    assert!(read_image.contains("workspace policy"));
}

#[test]
fn descriptions_do_not_reintroduce_repeated_operational_manuals() {
    let registry = build_default_registry();

    for tool_name in [
        "read_file",
        "write_file",
        "find_files",
        "file_info",
        "search_files",
        "edit_file",
        "bash",
        "check_background_command",
        "todo_write",
        "create_sub_task",
        "sub_task_status",
        "read_image",
        "task_finish",
        "ask_user",
        "activate_skill",
    ] {
        let text = description(&registry, tool_name);
        assert!(!text.contains("When to use:"), "{tool_name}: {text}");
        assert!(!text.contains("Guidelines:"), "{tool_name}: {text}");
        assert!(!text.contains("Protocol:"), "{tool_name}: {text}");
        assert!(!text.contains("Returns:"), "{tool_name}: {text}");
    }
}

#[test]
fn high_impact_parameters_retain_disambiguating_terms() {
    let registry = build_default_registry();

    for (tool_name, property_name, required_terms) in [
        (
            "bash",
            "stdin",
            &["interactive", "confirmation", "standard input"][..],
        ),
        ("search_files", "pattern", &["regex", "exact text"]),
        ("edit_file", "replace_all", &["all matches", "confirming"]),
        ("read_file", "start_line", &["1-based", "cursor"]),
        ("read_file", "end_line", &["inclusive", "cursor"]),
        (
            "create_sub_task",
            "exclude_files_pattern",
            &["portable regex", "child discovery"],
        ),
        (
            "sub_task_status",
            "workspace_file_limit",
            &["workspace files", "snapshot"],
        ),
        (
            "find_files",
            "include_sensitive",
            &["secrets", "credentials"],
        ),
        ("search_files", "case_sensitive", &["smart-case"]),
    ] {
        let text = property_description(&registry, tool_name, property_name).to_lowercase();
        for term in required_terms {
            assert!(
                text.contains(&term.to_lowercase()),
                "{tool_name}.{property_name} should mention {term}: {text}"
            );
        }
    }
}
