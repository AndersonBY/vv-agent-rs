use serde_json::{json, Value};

const LIST_FILES_DESCRIPTION: &str = r#"List files in workspace with optional path and glob filtering.

Large results are truncated, and common dependency/cache directories
(like node_modules/.venv) are summarized by default when listing from workspace root.

When to use:
- Discover repository structure, find candidate files before reading them, or inspect generated output locations.
- Use `path` to narrow the search root and `glob` to narrow file names before broad scans.
- Set `include_hidden=true` or `include_ignored=true` only when the task specifically needs those normally skipped paths.

Narrow first:
- Common dependency/cache directories such as node_modules, .venv, target, and build outputs are summarized from workspace-root listings by default.
- Large results are truncated; use the returned `truncated`, `returned_count`, `max_results`, `remaining_count`, and `ignored_roots` fields to choose a smaller follow-up query.
- When a backend scan limit is reached, the response can include `count_is_estimate=true`.

Returns:
- A structured list of normalized file paths plus counts, truncation metadata, ignored root summaries, and scan-limit hints.
- Errors for invalid paths or paths outside the permitted workspace."#;

pub(in crate::tools::schemas) fn list_files_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "list_files",
            "description": LIST_FILES_DESCRIPTION,
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
