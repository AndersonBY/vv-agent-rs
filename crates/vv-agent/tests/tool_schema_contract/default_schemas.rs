use serde_json::json;
use vv_agent::build_default_registry;

use super::helpers::{
    assert_description_contains, assert_nested_property_contains, assert_property_contains,
    description, nested_property_description, property_description,
};

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
    assert!(create_sub_task.contains("Delegation rules:"));
    assert!(create_sub_task.contains("Do not use batch mode"));
    assert!(create_sub_task.contains("Result handling:"));
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
    assert!(nested_property_description(
        &registry,
        "create_sub_task",
        &["tasks", "items", "output_requirements"]
    )
    .contains("verification evidence"));

    let sub_task_status = description(&registry, "sub_task_status");
    assert!(sub_task_status.contains("Capabilities:"));
    assert!(sub_task_status.contains("Continuation rules:"));
    assert!(sub_task_status.contains("detail_level=snapshot"));
    assert!(sub_task_status.contains("first task id"));
    assert!(sub_task_status.contains("continue a completed one"));
    assert!(sub_task_status.contains("max_cycles"));
    assert!(sub_task_status.contains("Do not continue a child task stopped at `max_cycles`"));
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
