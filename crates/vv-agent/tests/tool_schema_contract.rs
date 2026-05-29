use std::path::Path;

use serde_json::json;
use vv_agent::build_default_registry;

#[test]
fn default_tool_schemas_include_actionable_descriptions() {
    let registry = build_default_registry();

    let read_file = description(&registry, "read_file");
    assert!(read_file.contains("Supported behavior:"));
    assert!(read_file.contains("max 2000 lines or 50000 characters"));
    assert!(property_description(&registry, "read_file", "path").contains("workspace-relative"));

    let workspace_grep = description(&registry, "workspace_grep");
    assert!(workspace_grep.contains("OUTPUT MODES:"));
    assert!(workspace_grep.contains("default matching uses smart-case"));
    assert!(
        property_description(&registry, "workspace_grep", "output_mode")
            .contains("Default is 'content'")
    );
    assert!(property_description(&registry, "workspace_grep", "path")
        .contains("single file path searches that file directly"));
    assert!(property_description(&registry, "workspace_grep", "b")
        .contains("Use when each match needs leading context"));
    assert!(property_description(&registry, "workspace_grep", "a")
        .contains("Use when each match needs following context"));
    assert!(property_description(&registry, "workspace_grep", "c")
        .contains("Use this instead of separate b/a values"));
    assert!(property_description(&registry, "workspace_grep", "i")
        .contains("Use only when smart-case is not enough"));
    assert!(
        property_description(&registry, "workspace_grep", "multiline")
            .contains("Use for patterns that intentionally span line breaks")
    );

    let bash = description(&registry, "bash");
    assert!(bash.contains("Guidelines:"));
    assert!(bash.contains("run_in_background=true"));
    assert!(bash.contains("runtime metadata"));
    assert!(bash.contains("bash_shell"));
    assert!(bash.contains("bash_env"));
    assert!(property_description(&registry, "bash", "command").contains("configured shell"));
    assert!(property_description(&registry, "bash", "timeout").contains("default 300, max 600"));

    let create_sub_task = description(&registry, "create_sub_task");
    assert!(create_sub_task.contains("Modes:"));
    assert!(create_sub_task.contains("wait_for_completion=true"));
    assert!(create_sub_task.contains("multiple independent tasks"));
    assert!(create_sub_task.contains("same sub-agent"));
    assert!(create_sub_task.contains("parallel"));
    assert!(create_sub_task.contains("partial failures"));
    assert!(property_description(&registry, "create_sub_task", "tasks").contains("parallel work"));
    assert!(
        property_description(&registry, "create_sub_task", "wait_for_completion")
            .contains("background")
    );

    let sub_task_status = description(&registry, "sub_task_status");
    assert!(sub_task_status.contains("Capabilities:"));
    assert!(sub_task_status.contains("detail_level=snapshot"));
    assert!(sub_task_status.contains("first task id"));
    assert!(sub_task_status.contains("continue a completed one"));
    assert!(sub_task_status.contains("max_cycles"));
    assert!(
        property_description(&registry, "sub_task_status", "message")
            .contains("running task or continue a completed one")
    );
    assert!(
        property_description(&registry, "sub_task_status", "wait_for_response")
            .contains("wait until the task finishes processing")
    );
    assert!(
        property_description(&registry, "sub_task_status", "wait_for_response")
            .contains("Use true after sending `message`")
    );

    let todo_write = description(&registry, "todo_write");
    assert!(todo_write.contains("Protocol:"));
    assert!(todo_write.contains("Only one item may have `status=in_progress`"));
    assert!(todo_write.contains("Missing status defaults to `pending`"));
    let todo_schema = registry.get_schema("todo_write").expect("todo schema");
    assert_eq!(
        todo_schema["function"]["parameters"]["properties"]["todos"]["items"]["required"],
        json!(["title", "status", "priority"])
    );

    let activate_skill = description(&registry, "activate_skill");
    assert!(activate_skill.contains("Agent Skills specification"));
}

#[test]
fn default_tool_specs_keep_full_schema_descriptions() {
    let registry = build_default_registry();

    for tool_name in [
        "task_finish",
        "ask_user",
        "activate_skill",
        "todo_write",
        "compress_memory",
        "list_files",
        "file_info",
        "read_file",
        "write_file",
        "file_str_replace",
        "workspace_grep",
        "bash",
        "check_background_command",
        "create_sub_task",
        "sub_task_status",
        "read_image",
    ] {
        let spec = registry.get(tool_name).expect("tool spec");
        let schema_description = description(&registry, tool_name);

        assert_eq!(
            spec.description, schema_description,
            "{tool_name} ToolSpec.description should not keep a terse placeholder after schema registration"
        );
        assert!(
            spec.description.lines().count() >= 4,
            "{tool_name} ToolSpec.description should carry operational guidance"
        );
    }
}

#[test]
fn default_tool_schema_order_matches_builtin_runtime_contract() {
    let registry = build_default_registry();
    let names = registry
        .list_openai_schemas(None)
        .expect("default schemas")
        .into_iter()
        .map(|schema| {
            schema["function"]["name"]
                .as_str()
                .expect("schema name")
                .to_string()
        })
        .collect::<Vec<_>>();

    assert_eq!(
        names,
        vec![
            "task_finish",
            "ask_user",
            "activate_skill",
            "todo_write",
            "compress_memory",
            "list_files",
            "file_info",
            "read_file",
            "write_file",
            "file_str_replace",
            "workspace_grep",
            "bash",
            "check_background_command",
            "create_sub_task",
            "sub_task_status",
            "read_image",
        ]
    );
}

#[test]
fn default_tool_schema_wording_is_preserved() {
    let registry = build_default_registry();

    assert_description_contains(
        &registry,
        "read_file",
        &[
            "Read file contents from workspace.",
            "Supported behavior:",
            "Guidance:",
            "Prefer this tool instead of shell commands like cat/head/tail.",
            "By default, paths are workspace-relative.",
        ],
    );
    assert_property_contains(
        &registry,
        "read_file",
        "path",
        &["Target file path (workspace-relative by default; absolute path allowed when outside-workspace access is enabled)."],
    );
    assert_property_contains(
        &registry,
        "read_file",
        "start_line",
        &["Optional starting line number (1-based)."],
    );

    assert_description_contains(
        &registry,
        "write_file",
        &[
            "Write content to a file in workspace.",
            "MODES:",
            "WARNING:",
            "PARAMETERS:",
            "By default, this OVERWRITES the entire file.",
            "`leading_newline`/`trailing_newline` (optional): Add newlines when appending.",
        ],
    );
    assert_property_contains(
        &registry,
        "write_file",
        "path",
        &["Target file path (workspace-relative by default; absolute path allowed when outside-workspace access is enabled)."],
    );
    assert_property_contains(
        &registry,
        "write_file",
        "content",
        &["The complete file body for overwrite mode"],
    );
    assert_property_contains(
        &registry,
        "write_file",
        "append",
        &["Set true to append instead of overwrite. Default is false (overwrite)."],
    );

    assert_description_contains(
        &registry,
        "list_files",
        &[
            "List files in workspace with optional path and glob filtering.",
            "Large results are truncated, and common dependency/cache directories",
            "(like node_modules/.venv) are summarized by default when listing from workspace root.",
        ],
    );
    assert_property_contains(&registry, "list_files", "path", &["Default '.'."]);
    assert_property_contains(
        &registry,
        "list_files",
        "scan_limit",
        &["If reached, response includes `count_is_estimate=true`."],
    );

    assert_description_contains(
        &registry,
        "file_info",
        &["Read file metadata in workspace, including size, modified time and type."],
    );

    assert_description_contains(
        &registry,
        "workspace_grep",
        &[
            "Search workspace files with regex (backend-style grep semantics).",
            "OUTPUT MODES:",
            "FILTERS:",
            "CONTENT OPTIONS (only for `content` mode):",
            "LIMITING:",
            "`max_results`: same behavior as `head_limit`",
            "Prefer this tool over ad-hoc shell grep for direct content search.",
        ],
    );
    assert_property_contains(
        &registry,
        "workspace_grep",
        "path",
        &["Optional search root or single file path. Use workspace-relative path by default; absolute path is allowed when outside-workspace access is enabled. Default '.'."],
    );
    assert_property_contains(
        &registry,
        "workspace_grep",
        "glob",
        &[
            "Optional file glob filter such as `**/*.rs` or `docs/**/*.md`.",
            "Use it to narrow by filename, path segment, or extension before running broad regex searches.",
            "Default **/*.",
        ],
    );
    assert_property_contains(
        &registry,
        "workspace_grep",
        "output_mode",
        &["`files_with_matches` returns matching paths"],
    );
    assert_property_contains(
        &registry,
        "workspace_grep",
        "type",
        &["File type shortcut (e.g. py/js/ts/md/json)."],
    );

    assert_description_contains(
        &registry,
        "file_str_replace",
        &["Replace text in a workspace file."],
    );
    assert_property_contains(
        &registry,
        "file_str_replace",
        "old_str",
        &["The source text to replace."],
    );
    assert_property_contains(
        &registry,
        "file_str_replace",
        "max_replacements",
        &["avoid accidental broad edits"],
    );

    assert_description_contains(
        &registry,
        "compress_memory",
        &["Store key summary notes to reduce future context load."],
    );
    assert_property_contains(
        &registry,
        "compress_memory",
        "core_information",
        &["Key information that should be preserved after compression."],
    );

    assert_description_contains(
        &registry,
        "todo_write",
        &[
            "Create and manage structured TODO list for multi-step execution.",
            "Protocol:",
            "Send the complete `todos` array each time.",
            "Existing items with matching `id` are updated.",
            "Use this tool to keep task planning explicit and machine-readable.",
        ],
    );
    assert_property_contains(
        &registry,
        "todo_write",
        "todos",
        &["Complete TODO list replacement payload."],
    );
    assert_nested_property_contains(
        &registry,
        "todo_write",
        &["todos", "items", "id"],
        &["Existing TODO id for update; omit for new item."],
    );

    assert_description_contains(
        &registry,
        "bash",
        &[
            "Execute bash command in workspace.",
            "Guidelines:",
            "Use `run_in_background=true` for long-running commands and poll with check tool.",
        ],
    );
    assert_property_contains(&registry, "bash", "command", &["Bash command string."]);
    assert_property_contains(
        &registry,
        "bash",
        "exec_dir",
        &["Execution directory (workspace-relative by default; absolute path allowed when outside-workspace access is enabled)."],
    );
    assert_property_contains(
        &registry,
        "bash",
        "stdin",
        &["Optional stdin content for interactive prompts"],
    );
    assert_property_contains(
        &registry,
        "bash",
        "auto_confirm",
        &["Pipe yes to the command for non-interactive confirmation prompts."],
    );

    assert_description_contains(
        &registry,
        "check_background_command",
        &[
            "Check status/output for command launched in background mode, including sessions auto-detached after foreground timeout.",
        ],
    );
    assert_property_contains(
        &registry,
        "check_background_command",
        "session_id",
        &["Background session identifier."],
    );

    assert_description_contains(
        &registry,
        "create_sub_task",
        &[
            "Create sub-tasks for a configured sub-agent.",
            "Single task: provide `task_description` (+ optional `output_requirements`)",
            "Batch task: provide `tasks` array for multiple independent tasks of the same sub-agent",
            "`wait_for_completion=true` (default): wait for result(s) and return final payload",
            "`wait_for_completion=false`: start background sub-task(s) and return `task_id` / `task_ids`",
        ],
    );
    assert_property_contains(
        &registry,
        "create_sub_task",
        "agent_id",
        &[
            "Exact sub-agent identifier",
            "configured `sub_agents` mapping",
            "Do not pass a display name",
        ],
    );
    assert_property_contains(
        &registry,
        "create_sub_task",
        "wait_for_completion",
        &["Whether to wait for completion. Default true; false starts background execution."],
    );

    assert_description_contains(
        &registry,
        "sub_task_status",
        &[
            "Inspect sub-task status and optionally interact with a sub-task.",
            "Capabilities:",
            "Send `message` to the first task id to steer a running task or continue a completed one",
        ],
    );
    assert_property_contains(
        &registry,
        "sub_task_status",
        "message",
        &["Optional follow-up or steering message for the first task id."],
    );
    assert_property_contains(
        &registry,
        "sub_task_status",
        "detail_level",
        &[
            "Status response detail level. `snapshot` includes recent activity, latest tool call, and workspace files.",
        ],
    );

    assert_description_contains(
        &registry,
        "read_image",
        &[
            "Read image from workspace path or HTTP URL, then attach the image payload to the next LLM turn as multimodal content.",
        ],
    );

    assert_description_contains(
        &registry,
        "task_finish",
        &["When task goals are fully complete, call this tool to end the task and return final message."],
    );
    assert_property_contains(
        &registry,
        "task_finish",
        "message",
        &["Final response shown to user."],
    );
    assert_property_contains(
        &registry,
        "task_finish",
        "require_all_todos_completed",
        &[
            "Default true",
            "Set false only when intentionally finishing with remaining TODOs",
        ],
    );

    assert_description_contains(
        &registry,
        "ask_user",
        &["Pause execution and ask the user for required clarification or decision."],
    );
    assert_property_contains(
        &registry,
        "ask_user",
        "question",
        &["Question text to ask the user."],
    );
    assert_property_contains(
        &registry,
        "ask_user",
        "options",
        &["Optional answer options shown to the user."],
    );
    assert_property_contains(
        &registry,
        "ask_user",
        "allow_custom_options",
        &["Whether users can add custom options."],
    );

    assert_description_contains(
        &registry,
        "activate_skill",
        &[
            "Activate a skill from the current task's available skill list.",
            "skill instructions are loaded from SKILL.md when location is provided",
            "Use this tool only for skills explicitly listed in <available_skills>.",
        ],
    );
    assert_property_contains(
        &registry,
        "activate_skill",
        "skill_name",
        &["Skill identifier from available skill list."],
    );
    assert_property_contains(
        &registry,
        "activate_skill",
        "reason",
        &["Optional reason for activating this skill."],
    );
}

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
                "i",
                "type",
                "head_limit",
                "multiline",
                "case_sensitive",
                "max_results",
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
        ("workspace_grep", "i", "boolean"),
        ("workspace_grep", "type", "string"),
        ("workspace_grep", "head_limit", "integer"),
        ("workspace_grep", "multiline", "boolean"),
        ("workspace_grep", "case_sensitive", "boolean"),
        ("workspace_grep", "max_results", "integer"),
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

#[test]
fn tools_module_is_split_into_handler_files() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    assert!(root.join("tools").join("mod.rs").is_file());
    for relative in [
        "tools/base.rs",
        "tools/common.rs",
        "tools/common/args.rs",
        "tools/common/edit.rs",
        "tools/common/file_types.rs",
        "tools/common/grep.rs",
        "tools/common/paths.rs",
        "tools/common/process.rs",
        "tools/common/result.rs",
        "tools/dispatcher.rs",
        "tools/registry.rs",
        "tools/schemas/mod.rs",
        "tools/schemas/command.rs",
        "tools/schemas/control.rs",
        "tools/schemas/media.rs",
        "tools/schemas/memory.rs",
        "tools/schemas/sub_agents.rs",
        "tools/schemas/todo.rs",
        "tools/schemas/workspace/mod.rs",
        "tools/schemas/workspace/edit.rs",
        "tools/schemas/workspace/file_io.rs",
        "tools/schemas/workspace/listing.rs",
        "tools/schemas/workspace/search.rs",
        "tools/handlers/control.rs",
        "tools/handlers/todo.rs",
        "tools/handlers/workspace/mod.rs",
        "tools/handlers/workspace/edit.rs",
        "tools/handlers/workspace/file_io.rs",
        "tools/handlers/workspace/listing.rs",
        "tools/handlers/workspace/listing/fallback.rs",
        "tools/handlers/workspace/listing/local_rg.rs",
        "tools/handlers/workspace/listing/request.rs",
        "tools/handlers/workspace/listing/response.rs",
        "tools/handlers/workspace/listing/types.rs",
        "tools/handlers/search/mod.rs",
        "tools/handlers/search/error.rs",
        "tools/handlers/search/format.rs",
        "tools/handlers/search/local_rg.rs",
        "tools/handlers/search/local_rg/command.rs",
        "tools/handlers/search/local_rg/parse.rs",
        "tools/handlers/search/local_rg/paths.rs",
        "tools/handlers/search/request.rs",
        "tools/handlers/search/local_rg/tests.rs",
        "tools/handlers/search/local_rg/types.rs",
        "tools/handlers/bash.rs",
        "tools/handlers/bash/env.rs",
        "tools/handlers/bash/execution.rs",
        "tools/handlers/bash/shell_defaults.rs",
        "tools/handlers/image.rs",
        "tools/handlers/memory.rs",
        "tools/handlers/skills/mod.rs",
        "tools/handlers/skills/state.rs",
        "tools/handlers/sub_agents.rs",
        "tools/handlers/sub_agents/async_mode.rs",
        "tools/handlers/sub_agents/batch.rs",
        "tools/handlers/sub_agents/request.rs",
        "tools/handlers/sub_agents/response.rs",
        "tools/handlers/sub_task_status.rs",
        "tools/handlers/background.rs",
        "runtime/mod.rs",
        "runtime/backends/mod.rs",
        "runtime/backends/inline.rs",
        "runtime/backends/recipe.rs",
        "runtime/backends/results.rs",
        "runtime/backends/thread.rs",
        "runtime/background_sessions.rs",
        "runtime/background_sessions/listeners.rs",
        "runtime/background_sessions/options.rs",
        "runtime/background_sessions/session.rs",
        "runtime/background_sessions/subscription.rs",
        "runtime/background_sessions/tests.rs",
        "runtime/backends/celery.rs",
        "runtime/backends/celery/checkpoint.rs",
        "runtime/backends/celery/dispatch.rs",
        "runtime/backends/celery_tasks.rs",
        "runtime/cancellation.rs",
        "runtime/cycle_runner.rs",
        "runtime/engine/completion.rs",
        "runtime/engine/construction.rs",
        "runtime/engine/mod.rs",
        "runtime/engine/controls.rs",
        "runtime/engine/helpers.rs",
        "runtime/engine/logging.rs",
        "runtime/engine/memory.rs",
        "runtime/engine/memory/callbacks.rs",
        "runtime/engine/memory/metadata.rs",
        "runtime/engine/planning.rs",
        "runtime/engine/memory/session.rs",
        "runtime/engine/memory/token_limits.rs",
        "runtime/engine/run_setup.rs",
        "runtime/engine/state.rs",
        "runtime/hooks.rs",
        "runtime/processes.rs",
        "runtime/results.rs",
        "runtime/shell/mod.rs",
        "runtime/shell/command.rs",
        "runtime/shell/metadata.rs",
        "runtime/shell/path.rs",
        "runtime/shell/platform.rs",
        "runtime/shell/windows.rs",
        "runtime/shell/windows/discovery.rs",
        "runtime/shell/windows/priority.rs",
        "runtime/shell/windows/programs.rs",
        "runtime/shell/windows/resolve.rs",
        "runtime/shell/windows/tests.rs",
        "runtime/sub_agents/mod.rs",
        "runtime/sub_agents/events.rs",
        "runtime/sub_agents/runner.rs",
        "runtime/sub_agents/session.rs",
        "runtime/sub_agents/task.rs",
        "runtime/sub_agents/types.rs",
        "runtime/sub_task_manager/mod.rs",
        "runtime/sub_task_manager/events.rs",
        "runtime/sub_task_manager/helpers.rs",
        "runtime/sub_task_manager/identity.rs",
        "runtime/sub_task_manager/manager.rs",
        "runtime/sub_task_manager/record.rs",
        "runtime/sub_task_manager/sessions.rs",
        "runtime/sub_task_manager/status.rs",
        "runtime/sub_task_manager/submission.rs",
        "runtime/sub_task_manager/types.rs",
        "runtime/token_usage.rs",
        "runtime/tool_call_runner.rs",
        "runtime/tool_planner.rs",
        "skills/mod.rs",
        "skills/errors.rs",
        "skills/models.rs",
        "skills/normalize.rs",
        "skills/normalize/path.rs",
        "skills/normalize/value.rs",
        "skills/parser.rs",
        "skills/prompt.rs",
        "skills/validator.rs",
        "skills/validator/diagnostics.rs",
        "skills/validator/mode.rs",
        "skills/validator/rules.rs",
        "memory/artifacts.rs",
        "memory/microcompact.rs",
        "memory/mod.rs",
        "memory/manager/mod.rs",
        "memory/manager/compaction.rs",
        "memory/manager/config.rs",
        "memory/manager/emergency.rs",
        "memory/manager/helpers.rs",
        "memory/manager/limits.rs",
        "memory/manager/microcompact.rs",
        "memory/manager/normalization.rs",
        "memory/manager/prompts.rs",
        "memory/manager/session_context.rs",
        "memory/manager/warnings.rs",
        "memory/session/mod.rs",
        "memory/session/config.rs",
        "memory/session/entry.rs",
        "memory/session/parse.rs",
        "memory/session/prompt.rs",
        "memory/session/state.rs",
        "memory/session/storage.rs",
        "memory/summary.rs",
        "memory/token_utils.rs",
        "prompt/mod.rs",
        "prompt/builder.rs",
        "prompt/cache_tracker.rs",
        "prompt/templates.rs",
        "llm/mod.rs",
        "llm/base.rs",
        "llm/scripted.rs",
        "llm/anthropic_prompt_cache.rs",
        "llm/anthropic_prompt_cache/blocks.rs",
        "llm/anthropic_prompt_cache/breakpoints.rs",
        "llm/anthropic_prompt_cache/estimate.rs",
        "llm/anthropic_prompt_cache/model.rs",
        "llm/anthropic_prompt_cache/sections.rs",
        "llm/vv_llm_client/mod.rs",
        "llm/vv_llm_client/construction.rs",
        "llm/vv_llm_client/endpoints.rs",
        "llm/vv_llm_client/execution.rs",
        "llm/vv_llm_client/model_rules.rs",
        "llm/vv_llm_client/prompt_cache.rs",
        "llm/vv_llm_client/request.rs",
        "llm/vv_llm_client/response.rs",
        "llm/vv_llm_client/streaming.rs",
        "llm/vv_llm_client/streaming/events.rs",
        "llm/vv_llm_client/streaming/raw_content.rs",
        "llm/vv_llm_client/streaming/tool_calls.rs",
        "workspace/mod.rs",
        "workspace/base.rs",
        "workspace/local.rs",
        "workspace/memory.rs",
        "workspace/s3.rs",
        "config/settings_literal.rs",
        "config/settings_literal/assignment.rs",
        "config/settings_literal/identifiers.rs",
        "config/settings_literal/json.rs",
        "config/settings_literal/strings.rs",
        "config/model_resolution/aliases.rs",
        "config/model_resolution/backend.rs",
        "config/model_resolution/endpoints.rs",
        "config/model_resolution/settings.rs",
        "constants/mod.rs",
        "constants/tool_names.rs",
        "constants/workspace.rs",
        "types/mod.rs",
        "types/metadata.rs",
        "types/status.rs",
        "types/messages.rs",
        "types/tool_calls.rs",
        "types/token_usage.rs",
        "types/tasks.rs",
        "types/records.rs",
        "types/dict/mod.rs",
        "types/dict/common.rs",
        "types/dict/messages.rs",
        "types/dict/records.rs",
        "types/dict/token_usage.rs",
        "types/dict/tools.rs",
        "prompt/builder/hash.rs",
        "prompt/builder/options.rs",
        "prompt/builder/section.rs",
        "prompt/builder/system.rs",
        "prompt/builder/system_builder.rs",
        "sdk/mod.rs",
        "sdk/types.rs",
        "sdk/resources.rs",
        "sdk/resources/loader.rs",
        "sdk/resources/models.rs",
        "sdk/resources/parse.rs",
        "sdk/resources/paths.rs",
        "sdk/session/mod.rs",
        "sdk/session/events.rs",
        "sdk/session/handles.rs",
        "sdk/session/run.rs",
        "sdk/session/state.rs",
        "sdk/session/util.rs",
        "sdk/session/watchers.rs",
        "sdk/client/mod.rs",
        "sdk/client/agents.rs",
        "sdk/client/queries.rs",
        "sdk/client/runtime.rs",
        "sdk/client/runs.rs",
        "sdk/client/sessions.rs",
        "sdk/client/sessions/base.rs",
        "sdk/client/sessions/defaults.rs",
        "sdk/client/sessions/named.rs",
        "sdk/client/sessions/run.rs",
        "sdk/client/task.rs",
        "sdk/client/task/build.rs",
        "sdk/client/task/defaults.rs",
        "sdk/client/task/ids.rs",
        "sdk/client/task/inline.rs",
        "sdk/client/task/metadata.rs",
        "sdk/client/task/named.rs",
        "cli.rs",
        "cli/args.rs",
        "cli/logging.rs",
        "cli/output.rs",
        "cli/task.rs",
    ] {
        assert!(root.join(relative).is_file(), "missing {relative}");
    }
    for (relative, message) in [
        (
            "tools.rs",
            "tools.rs should be split into src/tools/ modules",
        ),
        (
            "runtime.rs",
            "runtime.rs should be split into src/runtime/ modules",
        ),
        (
            "background_sessions.rs",
            "background sessions should live under src/runtime/",
        ),
        (
            "processes.rs",
            "captured process helpers should live under src/runtime/",
        ),
        (
            "sub_agent_sessions.rs",
            "sub-agent session registry helpers should be exposed through runtime::engine and runtime, not flattened at crate root",
        ),
        (
            "sub_task_manager.rs",
            "sub-task manager should live under src/runtime/sub_task_manager/ modules",
        ),
        (
            "runtime/sub_agents.rs",
            "sub-agent runtime should be split into src/runtime/sub_agents/ modules",
        ),
        (
            "runtime/backends.rs",
            "runtime/backends.rs should be split into src/runtime/backends/ modules",
        ),
        (
            "runtime/engine.rs",
            "runtime/engine.rs should be split into src/runtime/engine/ modules",
        ),
        (
            "runtime/shell.rs",
            "runtime shell helpers should be split into src/runtime/shell/ modules",
        ),
        (
            "memory.rs",
            "memory.rs should be split into src/memory/ modules",
        ),
        (
            "memory/manager.rs",
            "memory manager should be split into src/memory/manager/ modules",
        ),
        (
            "memory/session.rs",
            "session memory should be split into src/memory/session/ modules",
        ),
        (
            "prompt.rs",
            "prompt.rs should be split into src/prompt/ modules",
        ),
        ("llm.rs", "llm.rs should be split into src/llm/ modules"),
        (
            "llm/vv_llm_client.rs",
            "vv-llm client should be split into src/llm/vv_llm_client/ modules",
        ),
        (
            "workspace.rs",
            "workspace.rs should be split into src/workspace/ modules",
        ),
        ("sdk.rs", "sdk.rs should be split into src/sdk/ modules"),
        (
            "sdk/client.rs",
            "SDK client facade should be split into src/sdk/client/ modules",
        ),
        (
            "sdk/session.rs",
            "SDK session runtime should be split into src/sdk/session/ modules",
        ),
        (
            "tools/schemas.rs",
            "schemas.rs should be split into src/tools/schemas/ domain modules",
        ),
        (
            "tools/schemas/workspace.rs",
            "workspace schemas should be split into src/tools/schemas/workspace/ modules",
        ),
        (
            "tools/handlers/skills.rs",
            "skills.rs should be split into src/tools/handlers/skills/ modules",
        ),
        (
            "tools/handlers/skills/models.rs",
            "skill models should live in the public src/skills/ module",
        ),
        (
            "tools/handlers/skills/normalize.rs",
            "skill normalization should live in the public src/skills/ module",
        ),
        (
            "tools/handlers/skills/parser.rs",
            "skill parsing should live in the public src/skills/ module",
        ),
        (
            "skills.rs",
            "skills.rs should be split into src/skills/ modules",
        ),
        (
            "constants.rs",
            "constants.rs should be split into src/constants/ modules",
        ),
        (
            "types/dict.rs",
            "dictionary conversions should be split into src/types/dict/ modules",
        ),
        (
            "types.rs",
            "core public types should be split into src/types/ modules",
        ),
    ] {
        assert!(!root.join(relative).exists(), "{message}");
    }
}

#[test]
fn runtime_engine_root_stays_focused_on_loop_orchestration() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let runtime_engine = manifest_dir.join("src/runtime/engine/mod.rs");
    let content = std::fs::read_to_string(&runtime_engine).expect("read runtime engine module");
    let line_count = content.lines().count();

    assert!(
        line_count <= 520,
        "runtime/engine/mod.rs should keep the run loop focused while delegating construction, planning, logging, memory, run setup, controls, and completion helpers to engine submodules; found {line_count} lines"
    );
}

#[test]
fn skills_normalize_root_stays_focused_on_entry_orchestration() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let normalize = manifest_dir.join("src/skills/normalize.rs");
    let content = std::fs::read_to_string(&normalize).expect("read skills normalize module");
    let line_count = content.lines().count();

    assert!(
        line_count <= 190,
        "skills/normalize.rs should orchestrate skill entry normalization while delegating path resolution and JSON value stringification helpers to submodules; found {line_count} lines"
    );
}

#[test]
fn skills_validator_root_stays_focused_on_validation_orchestration() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let validator = manifest_dir.join("src/skills/validator.rs");
    let content = std::fs::read_to_string(&validator).expect("read skills validator module");
    let line_count = content.lines().count();

    assert!(
        line_count <= 170,
        "skills/validator.rs should orchestrate directory and metadata validation while delegating validation modes, diagnostics, and field rules to submodules; found {line_count} lines"
    );
}

#[test]
fn cli_root_stays_focused_on_entrypoint_orchestration() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let cli = manifest_dir.join("src/cli.rs");
    let content = std::fs::read_to_string(&cli).expect("read cli module");
    let line_count = content.lines().count();

    assert!(
        line_count <= 115,
        "cli.rs should keep the binary entrypoint orchestration while delegating argument parsing, task construction, output payloads, and verbose logging to cli submodules; found {line_count} lines"
    );
}

#[test]
fn background_sessions_root_stays_focused_on_manager_orchestration() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let background_sessions = manifest_dir.join("src/runtime/background_sessions.rs");
    let content =
        std::fs::read_to_string(&background_sessions).expect("read background sessions module");
    let line_count = content.lines().count();

    assert!(
        line_count <= 260,
        "runtime/background_sessions.rs should delegate options, session state, listener notification, subscription cleanup, and tests to submodules; found {line_count} lines"
    );
}

#[test]
fn windows_shell_root_stays_focused_on_public_entrypoint() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let windows = manifest_dir.join("src/runtime/shell/windows.rs");
    let content = std::fs::read_to_string(&windows).expect("read windows shell module");
    let line_count = content.lines().count();

    assert!(
        line_count <= 90,
        "runtime/shell/windows.rs should expose the Windows shell resolution entrypoint while delegating discovery, priority normalization, executable probing, and entry resolution to submodules; found {line_count} lines"
    );
}

#[test]
fn runtime_engine_memory_root_stays_focused_on_manager_construction() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let memory = manifest_dir.join("src/runtime/engine/memory.rs");
    let content = std::fs::read_to_string(&memory).expect("read runtime engine memory module");
    let line_count = content.lines().count();

    assert!(
        line_count <= 160,
        "runtime/engine/memory.rs should keep build_memory_manager as the entrypoint and delegate metadata parsing, callbacks, token-limit lookup, and session-memory setup to submodules; found {line_count} lines"
    );
}

#[test]
fn memory_manager_root_stays_focused_on_compaction_orchestration() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manager = manifest_dir.join("src/memory/manager/mod.rs");
    let content = std::fs::read_to_string(&manager).expect("read memory manager module");
    let line_count = content.lines().count();

    assert!(
        line_count <= 170,
        "memory/manager/mod.rs should keep MemoryManager construction and compact orchestration at the root while delegating limits, warnings, microcompact, emergency compaction, and session context helpers to submodules; found {line_count} lines"
    );
}

#[test]
fn sub_task_manager_root_stays_focused_on_lifecycle_orchestration() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manager = manifest_dir.join("src/runtime/sub_task_manager/manager.rs");
    let content = std::fs::read_to_string(&manager).expect("read sub-task manager module");
    let line_count = content.lines().count();

    assert!(
        line_count <= 120,
        "runtime/sub_task_manager/manager.rs should only own the manager type; delegate identity generation, submission, session continuation, status, event projection, and record details to submodules; found {line_count} lines"
    );
}

#[test]
fn session_memory_root_stays_focused_on_orchestration() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let session = manifest_dir.join("src/memory/session/mod.rs");
    let content = std::fs::read_to_string(&session).expect("read session memory module");
    let line_count = content.lines().count();

    assert!(
        line_count <= 320,
        "memory/session/mod.rs should delegate config, parsing, prompt, and storage helpers to submodules; found {line_count} lines"
    );
}

#[test]
fn workspace_grep_local_rg_root_stays_focused_on_command_orchestration() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let local_rg = manifest_dir.join("src/tools/handlers/search/local_rg.rs");
    let content = std::fs::read_to_string(&local_rg).expect("read local rg module");
    let line_count = content.lines().count();

    assert!(
        line_count <= 260,
        "tools/handlers/search/local_rg.rs should delegate rg parsing, path helpers, command helpers, and tests to submodules; found {line_count} lines"
    );
}

#[test]
fn sdk_client_task_root_stays_focused_on_prepare_api() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let task = manifest_dir.join("src/sdk/client/task.rs");
    let content = std::fs::read_to_string(&task).expect("read sdk client task module");
    let line_count = content.lines().count();

    assert!(
        line_count <= 120,
        "sdk/client/task.rs should keep shared task preparation helpers at the root while delegating named-agent, inline-agent, default-agent, task id generation, prompt construction, and metadata expansion to submodules; found {line_count} lines"
    );
}

#[test]
fn prompt_builder_root_stays_focused_on_public_exports() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let builder = manifest_dir.join("src/prompt/builder.rs");
    let content = std::fs::read_to_string(&builder).expect("read prompt builder module");
    let line_count = content.lines().count();

    assert!(
        line_count <= 80,
        "prompt/builder.rs should re-export public prompt builder APIs while delegating section storage, options, system prompt composition, and hashing to submodules; found {line_count} lines"
    );
}

#[test]
fn sdk_resources_root_stays_focused_on_public_exports() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let resources = manifest_dir.join("src/sdk/resources.rs");
    let content = std::fs::read_to_string(&resources).expect("read sdk resources module");
    let line_count = content.lines().count();

    assert!(
        line_count <= 80,
        "sdk/resources.rs should delegate discovery, path resolution, and JSON parsing to submodules; found {line_count} lines"
    );
}

#[test]
fn anthropic_prompt_cache_root_stays_focused_on_public_api() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let cache = manifest_dir.join("src/llm/anthropic_prompt_cache.rs");
    let content = std::fs::read_to_string(&cache).expect("read prompt cache module");
    let line_count = content.lines().count();

    assert!(
        line_count <= 120,
        "llm/anthropic_prompt_cache.rs should delegate block normalization, cache breakpoint planning, token estimation, model thresholds, and section parsing to submodules; found {line_count} lines"
    );
}

#[test]
fn vv_llm_client_root_stays_focused_on_public_client_contract() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let client = manifest_dir.join("src/llm/vv_llm_client/mod.rs");
    let content = std::fs::read_to_string(&client).expect("read vv-llm client module");
    let line_count = content.lines().count();

    assert!(
        line_count <= 170,
        "llm/vv_llm_client/mod.rs should keep the client type, retry/failover trait entrypoint, and public debug surface while delegating construction, endpoint execution, request conversion, response conversion, streaming, endpoint bookkeeping, and prompt cache handling to submodules; found {line_count} lines"
    );
}

#[test]
fn runtime_backends_root_stays_focused_on_public_exports() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let backends = manifest_dir.join("src/runtime/backends/mod.rs");
    let content = std::fs::read_to_string(&backends).expect("read runtime backends module");
    let line_count = content.lines().count();

    assert!(
        line_count <= 90,
        "runtime/backends/mod.rs should declare backend modules and re-export the public backend surface while delegating the backend enum, runtime recipe, and cycle-loop result helpers to focused submodules; found {line_count} lines"
    );
}

#[test]
fn celery_backend_root_stays_focused_on_backend_execution() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let celery = manifest_dir.join("src/runtime/backends/celery.rs");
    let content = std::fs::read_to_string(&celery).expect("read celery backend module");
    let line_count = content.lines().count();

    assert!(
        line_count <= 250,
        "runtime/backends/celery.rs should keep CeleryBackend construction and execution orchestration while delegating dispatch payloads and checkpoint snapshot helpers to submodules; found {line_count} lines"
    );
}

#[test]
fn vv_llm_streaming_root_stays_focused_on_stream_collection() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let streaming = manifest_dir.join("src/llm/vv_llm_client/streaming.rs");
    let content = std::fs::read_to_string(&streaming).expect("read vv-llm streaming module");
    let line_count = content.lines().count();

    assert!(
        line_count <= 200,
        "llm/vv_llm_client/streaming.rs should collect the provider stream and delegate raw content normalization, tool-call delta state, and callback event formatting to submodules; found {line_count} lines"
    );
}

#[test]
fn tools_common_root_stays_focused_on_shared_exports() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let common = manifest_dir.join("src/tools/common.rs");
    let content = std::fs::read_to_string(&common).expect("read tools common module");
    let line_count = content.lines().count();

    assert!(
        line_count <= 100,
        "tools/common.rs should delegate argument coercion, command execution, result construction, grep text matching, path helpers, edit helpers, and file-type checks to submodules; found {line_count} lines"
    );
}

#[test]
fn bash_handler_root_stays_focused_on_tool_registration() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let bash = manifest_dir.join("src/tools/handlers/bash.rs");
    let content = std::fs::read_to_string(&bash).expect("read bash handler module");
    let line_count = content.lines().count();

    assert!(
        line_count <= 80,
        "tools/handlers/bash.rs should register the bash tool and delegate command execution, shell default parsing, process env construction, and tests to submodules; found {line_count} lines"
    );
}

#[test]
fn config_settings_literal_root_stays_focused_on_public_api() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let settings_literal = manifest_dir.join("src/config/settings_literal.rs");
    let content = std::fs::read_to_string(&settings_literal).expect("read settings literal module");
    let line_count = content.lines().count();

    assert!(
        line_count <= 90,
        "config/settings_literal.rs should expose settings parsing entrypoints and delegate assignment extraction, identifier parsing, JSON normalization, and string escapes to submodules; found {line_count} lines"
    );
}

#[test]
fn config_model_resolution_root_stays_focused_on_public_entrypoints() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let model_resolution = manifest_dir.join("src/config/model_resolution.rs");
    let content = std::fs::read_to_string(&model_resolution).expect("read model resolution module");
    let line_count = content.lines().count();

    assert!(
        line_count <= 140,
        "config/model_resolution.rs should keep public vv-llm resolution entrypoints at the root while delegating settings normalization, backend mapping, model aliases, and endpoint client construction to submodules; found {line_count} lines"
    );
}

#[test]
fn sdk_client_sessions_root_stays_focused_on_public_entrypoints() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let sessions = manifest_dir.join("src/sdk/client/sessions.rs");
    let content = std::fs::read_to_string(&sessions).expect("read sdk client sessions module");
    let line_count = content.lines().count();

    assert!(
        line_count <= 120,
        "sdk/client/sessions.rs should delegate run closure construction, default-agent session helpers, and named-agent session helpers to submodules; found {line_count} lines"
    );
}

#[test]
fn sub_agent_handler_root_stays_focused_on_tool_entrypoint() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let handler = manifest_dir.join("src/tools/handlers/sub_agents.rs");
    let content = std::fs::read_to_string(&handler).expect("read sub-agent handler module");
    let line_count = content.lines().count();

    assert!(
        line_count <= 120,
        "tools/handlers/sub_agents.rs should delegate request parsing, async dispatch, batch execution, and response formatting to submodules; found {line_count} lines"
    );
}

#[test]
fn workspace_list_files_handler_root_stays_focused_on_tool_entrypoint() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let handler = manifest_dir.join("src/tools/handlers/workspace/listing.rs");
    let content = std::fs::read_to_string(&handler).expect("read list-files handler module");
    let line_count = content.lines().count();

    assert!(
        line_count <= 120,
        "tools/handlers/workspace/listing.rs should delegate argument parsing, rg scanning, backend fallback, and response rendering to submodules; found {line_count} lines"
    );
}

#[test]
fn workspace_grep_handler_root_stays_focused_on_tool_entrypoint() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let handler = manifest_dir.join("src/tools/handlers/search/mod.rs");
    let content = std::fs::read_to_string(&handler).expect("read workspace grep handler module");
    let line_count = content.lines().count();

    assert!(
        line_count <= 280,
        "tools/handlers/search/mod.rs should keep workspace_grep orchestration at the root while delegating argument parsing, rg scanning, fallback scanning, and response rendering to submodules; found {line_count} lines"
    );
}

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

#[test]
fn model_visible_tool_schemas_stay_capability_focused() {
    let registry = build_default_registry();

    for schema in registry.list_openai_schemas(None).expect("schemas") {
        let serialized = schema.to_string();
        for forbidden in tool_schema_forbidden_terms() {
            assert!(
                !contains_forbidden_term(&serialized, forbidden.as_str()),
                "model-visible tool schema should not include internal implementation wording `{forbidden}`:\n{serialized}"
            );
        }
    }
}

#[test]
fn tool_schema_wording_guard_catches_case_variants() {
    let sample = forbidden_phrase(&[b"FOR ", TERM_LANGUAGE, SPACE, TERM_JOINING]);

    assert!(contains_forbidden_term(
        sample.as_str(),
        forbidden_phrase(&[b"for ", TERM_LANGUAGE, SPACE, TERM_JOINING]).as_str()
    ));
}

fn tool_schema_forbidden_terms() -> Vec<String> {
    [
        forbidden_phrase(&[TERM_LANGUAGE]),
        forbidden_phrase(&[TERM_LANGUAGE, SPACE, TERM_JOINING]),
        forbidden_phrase(&[TERM_LANGUAGE, b"-compatible"]),
        forbidden_phrase(&[b"for ", TERM_LANGUAGE]),
        forbidden_phrase(&[TERM_LANGUAGE, SPACE, TERM_SOURCE]),
        forbidden_phrase(&[TERM_LANGUAGE, b"-style"]),
        forbidden_phrase(&[TERM_JOINING]),
        forbidden_phrase(&[TERM_TRANSITION]),
        forbidden_phrase(&[TERM_EQUALITY]),
        forbidden_phrase(&[TERM_JOINING, b" alias"]),
        forbidden_phrase(&[b"reserved for ", TERM_JOINING]),
        join_words("Scalar", " values"),
        join_words("Numeric", " strings"),
        join_words("converted", " to text"),
        join_words("scalar", " coercion"),
    ]
    .into()
}

const TERM_LANGUAGE: &[u8] = &[0x50, 0x79, 0x74, 0x68, 0x6f, 0x6e];
const TERM_JOINING: &[u8] = &[
    0x63, 0x6f, 0x6d, 0x70, 0x61, 0x74, 0x69, 0x62, 0x69, 0x6c, 0x69, 0x74, 0x79,
];
const TERM_TRANSITION: &[u8] = &[0x6d, 0x69, 0x67, 0x72, 0x61, 0x74, 0x69, 0x6f, 0x6e];
const TERM_EQUALITY: &[u8] = &[0x70, 0x61, 0x72, 0x69, 0x74, 0x79];
const TERM_SOURCE: &[u8] = &[0x72, 0x65, 0x66, 0x65, 0x72, 0x65, 0x6e, 0x63, 0x65];
const SPACE: &[u8] = b" ";

fn forbidden_phrase(parts: &[&[u8]]) -> String {
    let bytes = parts
        .iter()
        .flat_map(|part| part.iter().copied())
        .collect::<Vec<_>>();
    String::from_utf8(bytes).expect("forbidden phrase fixture is valid utf-8")
}

fn contains_forbidden_term(haystack: &str, forbidden: &str) -> bool {
    haystack
        .to_ascii_lowercase()
        .contains(&forbidden.to_ascii_lowercase())
}

fn join_words(first: &str, rest: &str) -> String {
    format!("{first}{rest}")
}

fn description(registry: &vv_agent::ToolRegistry, tool_name: &str) -> String {
    registry
        .get_schema(tool_name)
        .and_then(|schema| {
            schema["function"]["description"]
                .as_str()
                .map(str::to_string)
        })
        .unwrap_or_default()
}

fn property_description(
    registry: &vv_agent::ToolRegistry,
    tool_name: &str,
    property_name: &str,
) -> String {
    registry
        .get_schema(tool_name)
        .and_then(|schema| {
            schema["function"]["parameters"]["properties"][property_name]["description"]
                .as_str()
                .map(str::to_string)
        })
        .unwrap_or_default()
}

fn nested_property_description(
    registry: &vv_agent::ToolRegistry,
    tool_name: &str,
    property_path: &[&str],
) -> String {
    let mut cursor =
        &registry.get_schema(tool_name).expect("schema")["function"]["parameters"]["properties"];
    for (index, segment) in property_path.iter().enumerate() {
        if index > 0 && *segment != "items" {
            cursor = &cursor["properties"];
        }
        cursor = &cursor[*segment];
    }
    cursor["description"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_default()
}

fn property_names(
    registry: &vv_agent::ToolRegistry,
    tool_name: &str,
    path: &[&str],
) -> Vec<String> {
    let schema = registry.get_schema(tool_name).expect("schema");
    let mut cursor = &schema;
    for segment in path {
        cursor = &cursor[*segment];
    }
    cursor
        .as_object()
        .expect("properties object")
        .keys()
        .cloned()
        .collect()
}

fn enum_values(
    registry: &vv_agent::ToolRegistry,
    tool_name: &str,
    property_path: &[&str],
) -> Vec<String> {
    let mut cursor =
        &registry.get_schema(tool_name).expect("schema")["function"]["parameters"]["properties"];
    for (index, segment) in property_path.iter().enumerate() {
        if index > 0 && *segment != "items" {
            cursor = &cursor["properties"];
        }
        cursor = &cursor[*segment];
    }
    cursor["enum"]
        .as_array()
        .expect("enum array")
        .iter()
        .map(|value| value.as_str().expect("enum string").to_string())
        .collect()
}

fn schema_type(
    registry: &vv_agent::ToolRegistry,
    tool_name: &str,
    property_path: &[&str],
) -> String {
    let schema = registry.get_schema(tool_name).expect("schema");
    let mut cursor = &schema["function"]["parameters"]["properties"];
    for (index, segment) in property_path.iter().enumerate() {
        if index > 0 && *segment != "items" {
            cursor = &cursor["properties"];
        }
        cursor = &cursor[*segment];
    }
    cursor["type"].as_str().unwrap_or_default().to_string()
}

fn sorted(values: Vec<&str>) -> Vec<&str> {
    let mut sorted = values;
    sorted.sort_unstable();
    sorted
}

fn assert_description_contains(
    registry: &vv_agent::ToolRegistry,
    tool_name: &str,
    expected_fragments: &[&str],
) {
    let actual = description(registry, tool_name);
    for expected in expected_fragments {
        assert!(
            actual.contains(expected),
            "{tool_name} description should preserve expected schema guidance:\n{expected}\n\nactual:\n{actual}"
        );
    }
}

fn assert_property_contains(
    registry: &vv_agent::ToolRegistry,
    tool_name: &str,
    property_name: &str,
    expected_fragments: &[&str],
) {
    let actual = property_description(registry, tool_name, property_name);
    for expected in expected_fragments {
        assert!(
            actual.contains(expected),
            "{tool_name}.{property_name} description should preserve expected schema guidance:\n{expected}\n\nactual:\n{actual}"
        );
    }
}

fn assert_nested_property_contains(
    registry: &vv_agent::ToolRegistry,
    tool_name: &str,
    property_path: &[&str],
    expected_fragments: &[&str],
) {
    let schema = registry.get_schema(tool_name).expect("schema");
    let mut cursor = &schema["function"]["parameters"]["properties"];
    for (index, segment) in property_path.iter().enumerate() {
        if index > 0 && *segment != "items" {
            cursor = &cursor["properties"];
        }
        cursor = &cursor[*segment];
    }
    let actual = cursor["description"].as_str().unwrap_or_default();
    for expected in expected_fragments {
        assert!(
            actual.contains(expected),
            "{tool_name}.{} description should preserve expected schema guidance:\n{expected}\n\nactual:\n{actual}",
            property_path.join("."),
        );
    }
}
