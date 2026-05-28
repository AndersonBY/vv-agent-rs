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
        &["The content to write to the file."],
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
        &["Optional file glob filter. Default **/*."],
    );
    assert_property_contains(
        &registry,
        "workspace_grep",
        "output_mode",
        &["Search output mode. Default is 'content'."],
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
        &["Optional cap when replace_all=false. Default 1."],
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
        &["Sub-agent identifier from configured sub_agents mapping."],
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
        ("task_finish", vec!["message", "exposed_files"]),
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
        "tools/handlers/workspace_io.rs",
        "tools/handlers/search.rs",
        "tools/handlers/bash.rs",
        "tools/handlers/image.rs",
        "tools/handlers/memory.rs",
        "tools/handlers/skills/mod.rs",
        "tools/handlers/skills/state.rs",
        "tools/handlers/sub_agents.rs",
        "tools/handlers/sub_task_status.rs",
        "tools/handlers/background.rs",
        "runtime/mod.rs",
        "runtime/backends/mod.rs",
        "runtime/backends/inline.rs",
        "runtime/backends/thread.rs",
        "runtime/background_sessions.rs",
        "runtime/backends/celery.rs",
        "runtime/backends/celery_tasks.rs",
        "runtime/cancellation.rs",
        "runtime/cycle_runner.rs",
        "runtime/engine.rs",
        "runtime/hooks.rs",
        "runtime/processes.rs",
        "runtime/results.rs",
        "runtime/shell.rs",
        "runtime/sub_agents.rs",
        "runtime/sub_task_manager.rs",
        "runtime/token_usage.rs",
        "runtime/tool_call_runner.rs",
        "runtime/tool_planner.rs",
        "skills/mod.rs",
        "skills/errors.rs",
        "skills/models.rs",
        "skills/normalize.rs",
        "skills/parser.rs",
        "skills/prompt.rs",
        "skills/validator.rs",
        "memory/artifacts.rs",
        "memory/microcompact.rs",
        "memory/mod.rs",
        "memory/manager.rs",
        "memory/session.rs",
        "memory/summary.rs",
        "memory/token_utils.rs",
        "prompt/mod.rs",
        "prompt/builder.rs",
        "prompt/cache_tracker.rs",
        "prompt/templates.rs",
        "llm/mod.rs",
        "llm/base.rs",
        "llm/scripted.rs",
        "llm/vv_llm_client.rs",
        "workspace/mod.rs",
        "workspace/base.rs",
        "workspace/local.rs",
        "workspace/memory.rs",
        "workspace/s3.rs",
        "constants/mod.rs",
        "constants/tool_names.rs",
        "constants/workspace.rs",
        "sdk/mod.rs",
        "sdk/types.rs",
        "sdk/resources.rs",
        "sdk/session.rs",
        "sdk/client.rs",
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
            "sub-task manager should live under src/runtime/",
        ),
        (
            "runtime/backends.rs",
            "runtime/backends.rs should be split into src/runtime/backends/ modules",
        ),
        (
            "memory.rs",
            "memory.rs should be split into src/memory/ modules",
        ),
        (
            "prompt.rs",
            "prompt.rs should be split into src/prompt/ modules",
        ),
        ("llm.rs", "llm.rs should be split into src/llm/ modules"),
        (
            "workspace.rs",
            "workspace.rs should be split into src/workspace/ modules",
        ),
        ("sdk.rs", "sdk.rs should be split into src/sdk/ modules"),
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
    ] {
        assert!(!root.join(relative).exists(), "{message}");
    }
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
fn model_visible_tool_schemas_stay_capability_focused() {
    let registry = build_default_registry();

    for schema in registry.list_openai_schemas(None).expect("schemas") {
        let serialized = schema.to_string();
        for forbidden in tool_schema_forbidden_terms() {
            assert!(
                !serialized.contains(forbidden.as_str()),
                "model-visible tool schema should not include internal implementation wording `{forbidden}`:\n{serialized}"
            );
        }
    }
}

fn tool_schema_forbidden_terms() -> Vec<String> {
    [
        forbidden_phrase(&[LANG, SPACE, COMPAT]),
        forbidden_phrase(&[LANG, b"-compatible"]),
        forbidden_phrase(&[b"for ", LANG]),
        forbidden_phrase(&[LANG, SPACE, REFERENCE]),
        forbidden_phrase(&[LANG, b"-style"]),
        forbidden_phrase(&[COMPAT, b" alias"]),
        forbidden_phrase(&[b"reserved for ", COMPAT]),
        join_words("Scalar", " values"),
        join_words("Numeric", " strings"),
        join_words("converted", " to text"),
        join_words("scalar", " coercion"),
    ]
    .into()
}

const LANG: &[u8] = &[0x50, 0x79, 0x74, 0x68, 0x6f, 0x6e];
const COMPAT: &[u8] = &[
    0x63, 0x6f, 0x6d, 0x70, 0x61, 0x74, 0x69, 0x62, 0x69, 0x6c, 0x69, 0x74, 0x79,
];
const REFERENCE: &[u8] = &[0x72, 0x65, 0x66, 0x65, 0x72, 0x65, 0x6e, 0x63, 0x65];
const SPACE: &[u8] = b" ";

fn forbidden_phrase(parts: &[&[u8]]) -> String {
    let bytes = parts
        .iter()
        .flat_map(|part| part.iter().copied())
        .collect::<Vec<_>>();
    String::from_utf8(bytes).expect("forbidden phrase fixture is valid utf-8")
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
