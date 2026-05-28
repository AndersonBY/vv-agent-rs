use serde_json::{json, Value};

const WORKSPACE_GREP_DESCRIPTION: &str = r#"Search workspace files with regex (backend-style grep semantics).

When to use:
- Find symbols, text, config keys, error strings, TODOs, or call sites before deciding which files to read or edit.
- Prefer this tool over ad-hoc shell grep for direct content search.
- Narrow broad searches with `path`, `glob`, or `type` so results stay useful and fast.

OUTPUT MODES:
- `content` (default): show matching lines with optional context and line numbers.
- `files_with_matches`: show only matching file paths.
- `count`: show per-file match counts.

FILTERS:
- `path` + `glob`: scope the search root and file pattern.
- A single file path searches that file directly, even if it is hidden or under an ignored root.
- `type`: language/file-type shortcut (py/js/ts/md/json/...).
- default matching uses smart-case: all-lowercase patterns search case-insensitively and patterns containing uppercase stay case-sensitive.
- `i`: force case-insensitive search.
- `multiline`: let `.` match newlines and allow multi-line patterns.
- `include_hidden`: include hidden files/directories.
- `include_ignored`: include common dependency/cache roots at workspace root.

CONTENT OPTIONS (only for `content` mode):
- `b`: lines before each match.
- `a`: lines after each match.
- `c`: lines before+after and overrides b/a.
- `n`: include line numbers.

LIMITING:
- `head_limit`: return only first N output rows/entries
- `max_results`: same behavior as `head_limit`

Returns:
- Matching content rows, file paths, or counts according to `output_mode`.
- Truncation metadata such as `content_truncated`, `structured_truncated`, `structured_item_limit`, and `structured_char_limit` when output is capped.
- `head_limit` and `max_results` both limit the first N output rows or entries."#;

pub(in crate::tools::schemas) fn workspace_grep_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "workspace_grep",
            "description": WORKSPACE_GREP_DESCRIPTION,
            "parameters": {
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Regex pattern to search for; escape regex metacharacters when searching for literal text such as dots, brackets, or file extensions."},
                    "path": {"type": "string", "description": "Optional search root or single file path. Use workspace-relative path by default; absolute path is allowed when outside-workspace access is enabled. Default '.'. A single file path searches that file directly, even if it is hidden or under an ignored root."},
                    "glob": {"type": "string", "description": "Optional file glob filter. Default **/*."},
                    "include_hidden": {"type": "boolean", "description": "Whether hidden files are included. Default false."},
                    "include_ignored": {"type": "boolean", "description": "When searching workspace root, include files under common dependency/cache directories. Default false."},
                    "output_mode": {"type": "string", "enum": ["content", "files_with_matches", "count"], "description": "Search output mode. Default is 'content'."},
                    "b": {"type": "integer", "description": "Lines before each match. Only used in content mode."},
                    "a": {"type": "integer", "description": "Lines after each match. Only used in content mode."},
                    "c": {"type": "integer", "description": "Context lines before and after each match. Overrides b/a."},
                    "n": {"type": "boolean", "description": "Whether to include line numbers in content output. Default true."},
                    "i": {"type": "boolean", "description": "Force case-insensitive search."},
                    "type": {"type": "string", "description": "File type shortcut (e.g. py/js/ts/md/json). Unsupported or unknown shortcuts return a structured error listing supported values."},
                    "head_limit": {"type": "integer", "minimum": 1, "description": "Limit to first N output rows/entries."},
                    "multiline": {"type": "boolean", "description": "Enable multiline regex mode."},
                    "case_sensitive": {"type": "boolean", "description": "Explicitly override smart-case behavior and `i`."},
                    "max_results": {"type": "integer", "minimum": 1, "description": "Same behavior as `head_limit`."}
                },
                "required": ["pattern"]
            }
        }
    })
}
