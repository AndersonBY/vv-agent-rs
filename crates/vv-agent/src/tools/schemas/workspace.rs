use serde_json::{json, Value};

pub(super) fn read_file_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "read_file",
            "description": "Read file contents from workspace.\n\nSupported behavior:\n- Reads plain UTF-8 text files and returns a content slice.\n- Uses 1-based line numbers for `start_line` and `end_line`.\n- Can prepend line numbers with `show_line_numbers=true`.\n- Enforces read limits per request: max 2000 lines or 50000 characters.\n- Large reads return file info payload instead of full content.\n\nGuidance:\n- Prefer this tool instead of shell commands like cat/head/tail.\n- For large files, read in chunks by line range.\n- By default, paths are workspace-relative.\n- If runtime metadata enables outside-workspace access, absolute local paths are allowed.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Target file path (workspace-relative by default; absolute path allowed when outside-workspace access is enabled)."},
                    "start_line": {"type": "integer", "minimum": 1, "description": "Optional starting line number (1-based). numeric string values are accepted for Python compatibility."},
                    "end_line": {"type": "integer", "minimum": 1, "description": "Optional ending line number (1-based, inclusive). numeric string values are accepted for Python compatibility."},
                    "show_line_numbers": {"type": "boolean", "description": "When true, prefixes each output line with its source line number."}
                },
                "required": ["path"]
            }
        }
    })
}

pub(super) fn write_file_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "write_file",
            "description": "Write content to a file in workspace.\n\nMODES:\n- Overwrite (default): Replaces entire file content.\n- Append: Adds to existing content (`append=true`).\n\nWARNING:\n- By default, this OVERWRITES the entire file.\n- Prefer `file_str_replace` for small or surgical edits to existing files.\n- Use `append=true` to add content instead of replacing content.\n\nBehavior:\n- This can create parent directories when the workspace backend supports them.\n- The result reports `written_chars`, `append`, and newline flags.\n\nPARAMETERS:\n- `path` (required): Workspace-relative path by default. Absolute path is allowed when outside-workspace access is enabled.\n- `content` (required): Content to write.\n- `append` (optional): Set true to append instead of overwrite.\n- `leading_newline`/`trailing_newline` (optional): Add newlines when appending.",
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

pub(super) fn list_files_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "list_files",
            "description": "List files in workspace with optional path and glob filtering.\n\nUse `path` to narrow the search root and `glob` to narrow file names before broad scans. Large results are truncated and report `truncated`, `returned_count`, `max_results`, and `remaining_count` so you can request a smaller scope. Common dependency/cache directories such as node_modules and .venv are skipped by default from workspace-root listings and summarized in `ignored_roots`; set `include_ignored=true` only when you specifically need those files.\n\nWhen a backend scan limit is reached, the response can include `count_is_estimate=true`.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Optional search root path. Use workspace-relative path by default; absolute path is allowed when outside-workspace access is enabled. Default '.'."},
                    "glob": {"type": "string", "description": "Optional glob pattern. Default **/*."},
                    "include_hidden": {"type": "boolean", "description": "Whether hidden files are included. Default false."},
                    "include_ignored": {"type": "boolean", "description": "When listing workspace root, include files under common dependency/cache directories. Default false."},
                    "max_results": {"type": "integer", "description": "Maximum number of file paths returned in one call. Default 500; larger values are capped. numeric string values are accepted for Python compatibility."},
                    "scan_limit": {"type": "integer", "description": "Maximum files scanned before stopping early to keep listing fast. If reached, response includes `count_is_estimate=true`. numeric string values are accepted for Python compatibility."}
                },
                "required": []
            }
        }
    })
}

pub(super) fn file_info_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "file_info",
            "description": "Inspect file metadata in workspace, including size, modified time, type, and line count when available.\n\nUse before reading large or binary files, before deciding chunk ranges for `read_file`, and when you need to check whether a path is a file or directory without loading file contents.",
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

pub(super) fn workspace_grep_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "workspace_grep",
            "description": "Search workspace files with regex (backend-style grep semantics).\n\nOUTPUT MODES:\n- `content` (default): show matching lines (supports context and line numbers)\n- `files_with_matches`: show only file paths\n- `count`: show per-file match counts\n\nFILTERS:\n- `path` + `glob`: scope the search root and file pattern\n- `type`: language/file-type shortcut (py/js/ts/md/json/...)\n- default matching uses smart-case: all-lowercase patterns search case-insensitively\n  and patterns containing uppercase stay case-sensitive\n- `i`: force case-insensitive search\n- `multiline`: let `.` match newlines and allow multi-line patterns\n- `include_hidden`: include hidden files/directories (default false)\n- `include_ignored`: include common dependency/cache roots at workspace root (default false)\n\nCONTENT OPTIONS (only for `content` mode):\n- `b`: lines before each match\n- `a`: lines after each match\n- `c`: lines before+after (overrides b/a)\n- `n`: include line numbers (default true)\n\nLIMITING:\n- `head_limit`: return only first N output rows/entries\n- `max_results`: compatibility alias for `head_limit`\n- text content is capped at 500 lines / 30k characters and reports `content_truncated`\n- structured metadata is capped separately to avoid very large tool payloads; check `structured_truncated`, `structured_item_limit`, and `structured_char_limit`\n\nGuidance:\n- Prefer this tool over ad-hoc shell grep for direct content search.\n- Narrow broad searches with `path`/`glob`/`type` for better performance.",
            "parameters": {
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Regex pattern to search for."},
                    "path": {"type": "string", "description": "Optional search root or single file path. A single file path searches that file directly, even if it is hidden or under an ignored root. Use workspace-relative path by default; absolute path is allowed when outside-workspace access is enabled. Default '.'."},
                    "glob": {"type": "string", "description": "Optional file glob filter. Default **/*."},
                    "include_hidden": {"type": "boolean", "description": "Whether hidden files are included. Default false."},
                    "include_ignored": {"type": "boolean", "description": "When searching workspace root, include files under common dependency/cache directories. Default false."},
                    "output_mode": {"type": "string", "enum": ["content", "files_with_matches", "count"], "description": "Search output mode. Default is 'content'."},
                    "b": {"type": "integer", "description": "Lines before each match. Only used in content mode. numeric string values are accepted for Python compatibility."},
                    "a": {"type": "integer", "description": "Lines after each match. Only used in content mode. numeric string values are accepted for Python compatibility."},
                    "c": {"type": "integer", "description": "Context lines before and after each match. Overrides b/a. numeric string values are accepted for Python compatibility."},
                    "n": {"type": "boolean", "description": "Whether to include line numbers in content output. Default true."},
                    "i": {"type": "boolean", "description": "Force case-insensitive search."},
                    "type": {"type": "string", "description": "File type shortcut (e.g. py/js/ts/md/json)."},
                    "head_limit": {"type": "integer", "minimum": 1, "description": "Limit to first N output rows/entries. numeric string values are accepted for Python compatibility."},
                    "multiline": {"type": "boolean", "description": "Enable multiline regex mode."},
                    "case_sensitive": {"type": "boolean", "description": "Explicitly override smart-case behavior and `i`."},
                    "max_results": {"type": "integer", "minimum": 1, "description": "Compatibility alias for `head_limit`. numeric string values are accepted for Python compatibility."}
                },
                "required": ["pattern"]
            }
        }
    })
}

pub(super) fn file_str_replace_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "file_str_replace",
            "description": "Replace text in a workspace file using exact `old_str` matching.\n\nEditing protocol:\n- Call `read_file` first so you can copy the exact surrounding text.\n- This is best for focused edits where a precise old/new string is safer than rewriting a whole file.\n- The operation fails if `old_str` is not found, which protects against stale assumptions.\n- By default only one match is replaced; use `replace_all=true` or `max_replacements` intentionally.\n\nPrefer this over shell-based sed/perl for repository edits because it returns structured failure details.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Target file path (workspace-relative by default; absolute path allowed when outside-workspace access is enabled)."},
                    "old_str": {"type": "string", "description": "The exact source text to replace. Include enough context to make the match unique."},
                    "new_str": {"type": "string", "description": "Replacement text."},
                    "replace_all": {"type": "boolean", "description": "Replace all matches when true. Default false."},
                    "max_replacements": {"type": "integer", "minimum": 1, "description": "Optional cap when replace_all=false. Default 1. numeric string values are accepted for Python compatibility."}
                },
                "required": ["path", "old_str", "new_str"]
            }
        }
    })
}
