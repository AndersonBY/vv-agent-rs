use std::collections::BTreeMap;

use serde_json::{json, Value};

pub const WORKSPACE_TOOLS: &[&str] = &[
    "list_files",
    "file_info",
    "read_file",
    "write_file",
    "file_str_replace",
    "workspace_grep",
    "compress_memory",
    "todo_write",
];

pub fn default_tool_schemas() -> BTreeMap<String, Value> {
    let mut schemas = BTreeMap::new();
    for (name, schema) in [
        ("task_finish", task_finish_schema()),
        ("ask_user", ask_user_schema()),
        ("activate_skill", activate_skill_schema()),
        ("read_file", read_file_schema()),
        ("write_file", write_file_schema()),
        ("list_files", list_files_schema()),
        ("file_info", file_info_schema()),
        ("workspace_grep", workspace_grep_schema()),
        ("file_str_replace", file_str_replace_schema()),
        ("compress_memory", compress_memory_schema()),
        ("todo_write", todo_write_schema()),
        ("bash", bash_schema()),
        (
            "check_background_command",
            check_background_command_schema(),
        ),
        ("create_sub_task", create_sub_task_schema()),
        ("sub_task_status", sub_task_status_schema()),
        ("read_image", read_image_schema()),
    ] {
        schemas.insert(name.to_string(), schema);
    }
    schemas
}

pub fn schema_for(name: &str) -> Option<Value> {
    default_tool_schemas().remove(name)
}

fn task_finish_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "task_finish",
            "description": "When task goals are fully complete, call this tool to end the task and return final message.",
            "parameters": {
                "type": "object",
                "properties": {
                    "message": {"type": "string", "description": "Final response shown to user."},
                    "exposed_files": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Optional output file paths that should be exposed as final deliverables."
                    }
                },
                "required": []
            }
        }
    })
}

fn ask_user_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "ask_user",
            "description": "Pause execution and ask the user for required clarification or decision.",
            "parameters": {
                "type": "object",
                "properties": {
                    "question": {"type": "string", "description": "Question text to ask the user."},
                    "options": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Optional answer options shown to the user."
                    },
                    "selection_type": {
                        "type": "string",
                        "enum": ["single", "multi"],
                        "description": "Single or multi-choice mode when options are provided."
                    },
                    "allow_custom_options": {
                        "type": "boolean",
                        "description": "Whether users can add custom options."
                    }
                },
                "required": ["question"]
            }
        }
    })
}

fn activate_skill_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "activate_skill",
            "description": "Activate a skill from the current task's available skill list.\n\nThe skill metadata follows the Agent Skills specification (https://github.com/agentskills/agentskills):\n- name/description are exposed in <available_skills>\n- skill instructions are loaded from SKILL.md when location is provided\n\nUse this tool only for skills explicitly listed in <available_skills>.",
            "parameters": {
                "type": "object",
                "properties": {
                    "skill_name": {"type": "string", "description": "Skill identifier from available skill list."},
                    "reason": {"type": "string", "description": "Optional reason for activating this skill."}
                },
                "required": ["skill_name"]
            }
        }
    })
}

fn read_file_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "read_file",
            "description": "Read file contents from workspace.\n\nSupported behavior:\n- Reads plain UTF-8 text files and returns a content slice.\n- Uses 1-based line numbers for `start_line` and `end_line`.\n- Can prepend line numbers with `show_line_numbers=true`.\n- Enforces read limits per request: max 2000 lines or 50000 characters.\n- Large reads return file info payload instead of full content.\n\nGuidance:\n- Prefer this tool instead of shell commands like cat/head/tail.\n- For large files, read in chunks by line range.\n- By default, paths are workspace-relative.\n- If runtime metadata enables outside-workspace access, absolute local paths are allowed.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Target file path (workspace-relative by default; absolute path allowed when outside-workspace access is enabled)."},
                    "start_line": {"type": "integer", "minimum": 1, "description": "Optional starting line number (1-based)."},
                    "end_line": {"type": "integer", "minimum": 1, "description": "Optional ending line number (1-based, inclusive)."},
                    "show_line_numbers": {"type": "boolean", "description": "When true, prefixes each output line with its source line number."}
                },
                "required": ["path"]
            }
        }
    })
}

fn write_file_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "write_file",
            "description": "Write content to a file in workspace.\n\nMODES:\n- Overwrite (default): Replaces entire file content.\n- Append: Adds to existing content (`append=true`).\n\nWARNING:\n- By default, this OVERWRITES the entire file.\n- Use `append=true` to add content instead.\n\nPARAMETERS:\n- `path` (required): Workspace-relative path by default. Absolute path is allowed when outside-workspace access is enabled.\n- `content` (required): Content to write.\n- `append` (optional): Set true to append instead of overwrite.\n- `leading_newline`/`trailing_newline` (optional): Add newlines when appending.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Target file path (workspace-relative by default; absolute path allowed when outside-workspace access is enabled)."},
                    "content": {"type": "string", "description": "The content to write to the file."},
                    "append": {"type": "boolean", "description": "Set true to append instead of overwrite. Default is false (overwrite)."},
                    "leading_newline": {"type": "boolean", "description": "Add a leading newline when appending. Default is false."},
                    "trailing_newline": {"type": "boolean", "description": "Add a trailing newline when appending. Default is false."}
                },
                "required": ["path", "content"]
            }
        }
    })
}

fn list_files_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "list_files",
            "description": "List files in workspace with optional path and glob filtering. Large results are truncated, and common dependency/cache directories (like node_modules/.venv) are summarized by default when listing from workspace root.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Optional search root path. Use workspace-relative path by default; absolute path is allowed when outside-workspace access is enabled. Default '.'."},
                    "glob": {"type": "string", "description": "Optional glob pattern. Default **/*."},
                    "include_hidden": {"type": "boolean", "description": "Whether hidden files are included. Default false."},
                    "include_ignored": {"type": "boolean", "description": "When listing workspace root, include files under common dependency/cache directories. Default false."},
                    "max_results": {"type": "integer", "description": "Maximum number of file paths returned in one call. Default 500; larger values are capped."},
                    "scan_limit": {"type": "integer", "description": "Maximum files scanned before stopping early to keep listing fast. If reached, response includes `count_is_estimate=true`."}
                },
                "required": []
            }
        }
    })
}

fn file_info_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "file_info",
            "description": "Read file metadata in workspace, including size, modified time and type.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Target file path (workspace-relative by default; absolute path allowed when outside-workspace access is enabled)."}
                },
                "required": ["path"]
            }
        }
    })
}

fn workspace_grep_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "workspace_grep",
            "description": "Search workspace files with regex (backend-style grep semantics).\n\nOUTPUT MODES:\n- `content` (default): show matching lines (supports context and line numbers)\n- `files_with_matches`: show only file paths\n- `count`: show per-file match counts\n\nFILTERS:\n- `path` + `glob`: scope the search root and file pattern\n- `type`: language/file-type shortcut (py/js/ts/md/json/...)\n- default matching uses smart-case: all-lowercase patterns search case-insensitively\n  and patterns containing uppercase stay case-sensitive\n- `i`: force case-insensitive search\n- `multiline`: let `.` match newlines and allow multi-line patterns\n- `include_hidden`: include hidden files/directories (default false)\n- `include_ignored`: include common dependency/cache roots at workspace root (default false)\n\nCONTENT OPTIONS (only for `content` mode):\n- `b`: lines before each match\n- `a`: lines after each match\n- `c`: lines before+after (overrides b/a)\n- `n`: include line numbers (default true)\n\nLIMITING:\n- `head_limit`: return only first N output rows/entries\n- `max_results`: compatibility alias for `head_limit`\n\nGuidance:\n- Prefer this tool over ad-hoc shell grep for direct content search.\n- Narrow broad searches with `path`/`glob`/`type` for better performance.",
            "parameters": {
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Regex pattern to search for."},
                    "path": {"type": "string", "description": "Optional search root or single file path. Use workspace-relative path by default; absolute path is allowed when outside-workspace access is enabled. Default '.'."},
                    "glob": {"type": "string", "description": "Optional file glob filter. Default **/*."},
                    "include_hidden": {"type": "boolean", "description": "Whether hidden files are included. Default false."},
                    "include_ignored": {"type": "boolean", "description": "When searching workspace root, include files under common dependency/cache directories. Default false."},
                    "output_mode": {"type": "string", "enum": ["content", "files_with_matches", "count"], "description": "Search output mode. Default is 'content'."},
                    "b": {"type": "integer", "description": "Lines before each match. Only used in content mode."},
                    "a": {"type": "integer", "description": "Lines after each match. Only used in content mode."},
                    "c": {"type": "integer", "description": "Context lines before and after each match. Overrides b/a."},
                    "n": {"type": "boolean", "description": "Whether to include line numbers in content output. Default true."},
                    "i": {"type": "boolean", "description": "Force case-insensitive search."},
                    "type": {"type": "string", "description": "File type shortcut (e.g. py/js/ts/md/json)."},
                    "head_limit": {"type": "integer", "minimum": 1, "description": "Limit to first N output rows/entries."},
                    "multiline": {"type": "boolean", "description": "Enable multiline regex mode."},
                    "case_sensitive": {"type": "boolean", "description": "Explicitly override smart-case behavior and `i`."},
                    "max_results": {"type": "integer", "minimum": 1, "description": "Compatibility alias for `head_limit`."}
                },
                "required": ["pattern"]
            }
        }
    })
}

fn file_str_replace_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "file_str_replace",
            "description": "Replace text in a workspace file.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Target file path (workspace-relative by default; absolute path allowed when outside-workspace access is enabled)."},
                    "old_str": {"type": "string", "description": "The source text to replace."},
                    "new_str": {"type": "string", "description": "Replacement text."},
                    "replace_all": {"type": "boolean", "description": "Replace all matches when true. Default false."},
                    "max_replacements": {"type": "integer", "minimum": 1, "description": "Optional cap when replace_all=false. Default 1."}
                },
                "required": ["path", "old_str", "new_str"]
            }
        }
    })
}

fn compress_memory_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "compress_memory",
            "description": "Store key summary notes to reduce future context load.",
            "parameters": {
                "type": "object",
                "properties": {
                    "core_information": {"type": "string", "description": "Key information that should be preserved after compression."}
                },
                "required": ["core_information"]
            }
        }
    })
}

fn todo_write_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "todo_write",
            "description": "Create and manage structured TODO list for multi-step execution.\n\nProtocol:\n- Send the complete `todos` array each time.\n- Existing items with matching `id` are updated.\n- Items omitted from the new array are removed.\n- Only one item may have `status=in_progress`.\n\nUse this tool to keep task planning explicit and machine-readable.",
            "parameters": {
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "description": "Complete TODO list payload.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": {"type": "string", "description": "Existing TODO id for update; omit for new item."},
                                "title": {"type": "string", "description": "TODO title."},
                                "status": {"type": "string", "enum": ["pending", "in_progress", "completed"], "description": "TODO status."},
                                "priority": {"type": "string", "enum": ["low", "medium", "high"], "description": "TODO priority."}
                            },
                            "required": ["title", "status", "priority"]
                        }
                    }
                },
                "required": ["todos"]
            }
        }
    })
}

fn bash_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "bash",
            "description": "Execute bash command in workspace.\n\nGuidelines:\n- Prefer specialized read/write/search/edit tools when possible.\n- Use this tool for command execution, package install, scripts, and piped workflows.\n- For commands that may prompt for confirmation, pass `auto_confirm=true` or provide explicit `stdin`.\n- Use `run_in_background=true` for long-running commands and poll with check tool.\n- If a foreground command hits its timeout, it is automatically moved to a background\n  session and returns a `session_id` for polling.",
            "parameters": {
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "Bash command string."},
                    "exec_dir": {"type": "string", "description": "Execution directory (workspace-relative by default; absolute path allowed when outside-workspace access is enabled)."},
                    "timeout": {"type": "integer", "description": "Timeout seconds, default 300, max 600."},
                    "stdin": {"type": "string", "description": "Optional stdin content."},
                    "auto_confirm": {"type": "boolean", "description": "Pipe yes to command when true."},
                    "run_in_background": {"type": "boolean", "description": "Run command in background and return session_id for polling."}
                },
                "required": ["command"]
            }
        }
    })
}

fn check_background_command_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "check_background_command",
            "description": "Check status/output for command launched in background mode, including sessions auto-detached after foreground timeout.",
            "parameters": {
                "type": "object",
                "properties": {"session_id": {"type": "string", "description": "Background session identifier."}},
                "required": ["session_id"]
            }
        }
    })
}

fn create_sub_task_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "create_sub_task",
            "description": "Create sub-tasks for a configured sub-agent.\n\nModes:\n- Single task: provide `task_description` (+ optional `output_requirements`)\n- Batch task: provide `tasks` array for multiple independent tasks of the same sub-agent\n\nExecution:\n- `wait_for_completion=true` (default): wait for result(s) and return final payload\n- `wait_for_completion=false`: start background sub-task(s) and return `task_id` / `task_ids`\n\nUse `sub_task_status` later to inspect progress, fetch results, or send follow-up messages.",
            "parameters": {
                "type": "object",
                "properties": {
                    "agent_id": {"type": "string", "description": "Sub-agent identifier from configured sub_agents mapping."},
                    "task_description": {"type": "string", "description": "Single-task description. Mutually exclusive with `tasks`."},
                    "output_requirements": {"type": "string", "description": "Optional output constraints for single-task mode."},
                    "tasks": {
                        "type": "array",
                        "description": "Batch mode: multiple independent tasks for the same sub-agent.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "task_description": {"type": "string", "description": "Task description for one sub-task."},
                                "output_requirements": {"type": "string", "description": "Optional output constraints for one sub-task."}
                            },
                            "required": ["task_description"]
                        }
                    },
                    "include_main_summary": {"type": "boolean", "description": "Whether to include parent-task summary context. Default false."},
                    "exclude_files_pattern": {"type": "string", "description": "Optional regex for excluding files in shared context (reserved for compatibility)."},
                    "wait_for_completion": {"type": "boolean", "description": "Whether to wait for completion. Default true; false starts background execution."}
                },
                "required": ["agent_id"]
            }
        }
    })
}

fn sub_task_status_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "sub_task_status",
            "description": "Inspect sub-task status and optionally interact with a sub-task.\n\nCapabilities:\n- Query one or more sub-task ids\n- Return lightweight snapshot progress (`detail_level=snapshot`)\n- Send `message` to the first task id to steer a running task or continue a completed one\n- Optionally wait for the follow-up response with `wait_for_response=true`",
            "parameters": {
                "type": "object",
                "properties": {
                    "task_ids": {"type": "array", "description": "Sub-task ids to query. When `message` is provided, only the first id is used as the target.", "items": {"type": "string"}},
                    "message": {"type": "string", "description": "Optional follow-up or steering message for the first task id."},
                    "detail_level": {"type": "string", "enum": ["basic", "snapshot"], "description": "Status response detail level. `snapshot` includes recent activity, latest tool call, and workspace files."},
                    "workspace_file_limit": {"type": "integer", "minimum": 1, "maximum": 100, "description": "Maximum number of workspace files returned per task in snapshot mode. Default 20."},
                    "wait_for_response": {"type": "boolean", "description": "When `message` is provided, wait until the task finishes processing that message."}
                },
                "required": ["task_ids"]
            }
        }
    })
}

fn read_image_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "read_image",
            "description": "Read image from workspace path or HTTP URL, then attach the image payload to the next LLM turn as multimodal content.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Image path (workspace-relative by default; absolute path allowed when outside-workspace access is enabled) or http(s) image URL."}
                },
                "required": ["path"]
            }
        }
    })
}
