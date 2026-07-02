use serde_json::json;
use vv_agent::build_default_registry;

use super::helpers::{
    description, enum_values, property_description, property_names, schema_type, sorted,
};

#[test]
fn builtin_tool_required_fields_match_agent_schema_contract() {
    let registry = build_default_registry();

    for (tool_name, expected_required) in [
        ("activate_skill", json!(["skill_name"])),
        ("ask_user", json!(["question"])),
        ("bash", json!(["command"])),
        ("check_background_command", json!(["session_id"])),
        ("compress_memory", json!(["core_information"])),
        ("create_sub_task", json!(["agent_id"])),
        ("file_info", json!(["path"])),
        ("file_str_replace", json!(["path", "old_str", "new_str"])),
        ("list_files", json!([])),
        ("read_file", json!(["path"])),
        ("read_image", json!(["path"])),
        ("sub_task_status", json!(["task_ids"])),
        ("task_finish", json!([])),
        ("todo_write", json!(["todos"])),
        ("workspace_grep", json!(["pattern"])),
        ("write_file", json!(["path", "content"])),
    ] {
        let schema = registry.get_schema(tool_name).expect("schema");
        assert_eq!(
            schema["function"]["parameters"]["required"], expected_required,
            "{tool_name} top-level required fields should match the agent schema contract"
        );
    }

    let create_sub_task = registry.get_schema("create_sub_task").expect("schema");
    assert_eq!(
        create_sub_task["function"]["parameters"]["properties"]["tasks"]["items"]["required"],
        json!(["task_description"]),
        "create_sub_task.tasks item required fields should match the agent schema contract"
    );

    let todo_write = registry.get_schema("todo_write").expect("schema");
    assert_eq!(
        todo_write["function"]["parameters"]["properties"]["todos"]["items"]["required"],
        json!(["title", "status", "priority"]),
        "todo_write.todos item required fields should match the agent schema contract"
    );
    assert!(
        description(&registry, "todo_write")
            .contains("Each item must include `title`, `status`, and `priority`"),
        "todo_write should guide the model to emit the required fields explicitly"
    );
}

#[test]
fn builtin_tool_properties_and_enums_match_agent_schema_contract() {
    let registry = build_default_registry();

    for (tool_name, expected_properties) in [
        ("activate_skill", vec!["skill_name", "reason"]),
        (
            "ask_user",
            vec![
                "question",
                "options",
                "selection_type",
                "allow_custom_options",
            ],
        ),
        (
            "bash",
            vec![
                "command",
                "exec_dir",
                "timeout",
                "stdin",
                "auto_confirm",
                "run_in_background",
            ],
        ),
        ("check_background_command", vec!["session_id"]),
        ("compress_memory", vec!["core_information"]),
        (
            "create_sub_task",
            vec![
                "agent_id",
                "task_description",
                "output_requirements",
                "tasks",
                "include_main_summary",
                "exclude_files_pattern",
                "wait_for_completion",
            ],
        ),
        ("file_info", vec!["path"]),
        (
            "file_str_replace",
            vec![
                "path",
                "old_str",
                "new_str",
                "replace_all",
                "max_replacements",
            ],
        ),
        (
            "list_files",
            vec![
                "path",
                "glob",
                "include_hidden",
                "include_ignored",
                "max_results",
                "scan_limit",
            ],
        ),
        (
            "read_file",
            vec!["path", "start_line", "end_line", "show_line_numbers"],
        ),
        ("read_image", vec!["path"]),
        (
            "sub_task_status",
            vec![
                "task_ids",
                "message",
                "detail_level",
                "workspace_file_limit",
                "wait_for_response",
                "wait_for_completion",
                "check_interval_seconds",
                "max_wait_seconds",
            ],
        ),
        (
            "task_finish",
            vec!["message", "require_all_todos_completed", "exposed_files"],
        ),
        ("todo_write", vec!["todos"]),
        (
            "workspace_grep",
            vec![
                "pattern",
                "path",
                "glob",
                "include_hidden",
                "include_ignored",
                "output_mode",
                "b",
                "a",
                "c",
                "n",
                "type",
                "head_limit",
                "multiline",
                "case_sensitive",
            ],
        ),
        (
            "write_file",
            vec![
                "path",
                "content",
                "append",
                "leading_newline",
                "trailing_newline",
            ],
        ),
    ] {
        let mut expected_properties = expected_properties;
        expected_properties.sort_unstable();
        assert_eq!(
            property_names(
                &registry,
                tool_name,
                &["function", "parameters", "properties"]
            ),
            expected_properties,
            "{tool_name} properties should match the agent schema contract"
        );
    }

    assert_eq!(
        enum_values(&registry, "ask_user", &["selection_type"]),
        vec!["single", "multi"]
    );
    assert_eq!(
        enum_values(&registry, "sub_task_status", &["detail_level"]),
        vec!["basic", "snapshot"]
    );
    assert_eq!(
        enum_values(&registry, "workspace_grep", &["output_mode"]),
        vec!["content", "files_with_matches", "count"]
    );
    assert_eq!(
        property_names(
            &registry,
            "create_sub_task",
            &[
                "function",
                "parameters",
                "properties",
                "tasks",
                "items",
                "properties",
            ],
        ),
        sorted(vec!["task_description", "output_requirements"])
    );
    assert!(!property_names(
        &registry,
        "create_sub_task",
        &["function", "parameters", "properties"]
    )
    .contains(&"agent_name".to_string()));
    assert_eq!(
        property_names(
            &registry,
            "todo_write",
            &[
                "function",
                "parameters",
                "properties",
                "todos",
                "items",
                "properties",
            ],
        ),
        sorted(vec!["id", "title", "status", "priority"])
    );
    assert_eq!(
        enum_values(&registry, "todo_write", &["todos", "items", "status"]),
        vec!["pending", "in_progress", "completed"]
    );
    assert_eq!(
        enum_values(&registry, "todo_write", &["todos", "items", "priority"]),
        vec!["low", "medium", "high"]
    );
}

#[test]
fn builtin_tool_property_types_match_agent_schema_contract() {
    let registry = build_default_registry();

    for (tool_name, property_name, expected_type) in [
        ("activate_skill", "skill_name", "string"),
        ("activate_skill", "reason", "string"),
        ("ask_user", "question", "string"),
        ("ask_user", "options", "array"),
        ("ask_user", "selection_type", "string"),
        ("ask_user", "allow_custom_options", "boolean"),
        ("bash", "command", "string"),
        ("bash", "exec_dir", "string"),
        ("bash", "timeout", "integer"),
        ("bash", "stdin", "string"),
        ("bash", "auto_confirm", "boolean"),
        ("bash", "run_in_background", "boolean"),
        ("check_background_command", "session_id", "string"),
        ("compress_memory", "core_information", "string"),
        ("create_sub_task", "agent_id", "string"),
        ("create_sub_task", "task_description", "string"),
        ("create_sub_task", "output_requirements", "string"),
        ("create_sub_task", "tasks", "array"),
        ("create_sub_task", "include_main_summary", "boolean"),
        ("create_sub_task", "exclude_files_pattern", "string"),
        ("create_sub_task", "wait_for_completion", "boolean"),
        ("file_info", "path", "string"),
        ("file_str_replace", "path", "string"),
        ("file_str_replace", "old_str", "string"),
        ("file_str_replace", "new_str", "string"),
        ("file_str_replace", "replace_all", "boolean"),
        ("file_str_replace", "max_replacements", "integer"),
        ("list_files", "path", "string"),
        ("list_files", "glob", "string"),
        ("list_files", "include_hidden", "boolean"),
        ("list_files", "include_ignored", "boolean"),
        ("list_files", "max_results", "integer"),
        ("list_files", "scan_limit", "integer"),
        ("read_file", "path", "string"),
        ("read_file", "start_line", "integer"),
        ("read_file", "end_line", "integer"),
        ("read_file", "show_line_numbers", "boolean"),
        ("read_image", "path", "string"),
        ("sub_task_status", "task_ids", "array"),
        ("sub_task_status", "message", "string"),
        ("sub_task_status", "detail_level", "string"),
        ("sub_task_status", "workspace_file_limit", "integer"),
        ("sub_task_status", "wait_for_response", "boolean"),
        ("sub_task_status", "wait_for_completion", "boolean"),
        ("sub_task_status", "check_interval_seconds", "integer"),
        ("task_finish", "message", "string"),
        ("task_finish", "require_all_todos_completed", "boolean"),
        ("task_finish", "exposed_files", "array"),
        ("todo_write", "todos", "array"),
        ("workspace_grep", "pattern", "string"),
        ("workspace_grep", "path", "string"),
        ("workspace_grep", "glob", "string"),
        ("workspace_grep", "include_hidden", "boolean"),
        ("workspace_grep", "include_ignored", "boolean"),
        ("workspace_grep", "output_mode", "string"),
        ("workspace_grep", "b", "integer"),
        ("workspace_grep", "a", "integer"),
        ("workspace_grep", "c", "integer"),
        ("workspace_grep", "n", "boolean"),
        ("workspace_grep", "type", "string"),
        ("workspace_grep", "head_limit", "integer"),
        ("workspace_grep", "multiline", "boolean"),
        ("workspace_grep", "case_sensitive", "boolean"),
        ("write_file", "path", "string"),
        ("write_file", "content", "string"),
        ("write_file", "append", "boolean"),
        ("write_file", "leading_newline", "boolean"),
        ("write_file", "trailing_newline", "boolean"),
    ] {
        assert_eq!(
            schema_type(&registry, tool_name, &[property_name]),
            expected_type,
            "{tool_name}.{property_name} type should match the agent schema contract"
        );
    }

    let sub_task_status = registry.get_schema("sub_task_status").expect("schema");
    assert_eq!(
        sub_task_status["function"]["parameters"]["properties"]["max_wait_seconds"]["type"],
        json!(["integer", "null"]),
        "sub_task_status.max_wait_seconds should accept an integer timeout or null"
    );

    for (tool_name, property_path, expected_type) in [
        ("ask_user", vec!["options", "items"], "string"),
        ("create_sub_task", vec!["tasks", "items"], "object"),
        (
            "create_sub_task",
            vec!["tasks", "items", "task_description"],
            "string",
        ),
        (
            "create_sub_task",
            vec!["tasks", "items", "output_requirements"],
            "string",
        ),
        ("sub_task_status", vec!["task_ids", "items"], "string"),
        ("task_finish", vec!["exposed_files", "items"], "string"),
        ("todo_write", vec!["todos", "items"], "object"),
        ("todo_write", vec!["todos", "items", "id"], "string"),
        ("todo_write", vec!["todos", "items", "title"], "string"),
        ("todo_write", vec!["todos", "items", "status"], "string"),
        ("todo_write", vec!["todos", "items", "priority"], "string"),
    ] {
        assert_eq!(
            schema_type(&registry, tool_name, &property_path),
            expected_type,
            "{tool_name}.{} type should match the agent schema contract",
            property_path.join(".")
        );
    }
}

#[test]
fn control_tool_parameter_descriptions_steer_high_quality_agent_decisions() {
    let registry = build_default_registry();

    let ask_user = description(&registry, "ask_user");
    assert!(ask_user.contains("When to use:"));
    assert!(ask_user.contains("Do not use this for facts"));
    assert!(ask_user.contains("blocks the runtime"));
    assert!(property_description(&registry, "ask_user", "question")
        .contains("the smallest decision needed to unblock progress"));
    assert!(property_description(&registry, "ask_user", "options").contains("2-3"));
    assert!(property_description(&registry, "ask_user", "options").contains("mutually exclusive"));
    assert!(
        property_description(&registry, "ask_user", "selection_type")
            .contains("Use `multi` only when")
    );
    assert!(
        property_description(&registry, "ask_user", "allow_custom_options")
            .contains("preset options may be incomplete")
    );

    let activate_skill = description(&registry, "activate_skill");
    assert!(activate_skill.contains("When to use:"));
    assert!(activate_skill.contains("Read the returned SKILL.md instructions"));
    assert!(activate_skill.contains("Do not invent"));
    assert!(
        property_description(&registry, "activate_skill", "skill_name").contains("exact `name`")
    );
    assert!(property_description(&registry, "activate_skill", "reason")
        .contains("why this skill applies before acting"));
}
