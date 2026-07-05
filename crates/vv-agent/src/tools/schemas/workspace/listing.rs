use serde_json::{json, Value};

const FIND_FILES_DESCRIPTION: &str = r#"Find files in workspace with optional path and glob filtering.

Large results are truncated, and common dependency/cache directories
(like node_modules/.venv) are summarized by default when listing from workspace root.

When to use:
- Discover repository structure, find candidate files before reading them, or inspect generated output locations.
- Use `path` to narrow the search root and `glob` to narrow file names before broad scans.
- Set `include_hidden=true` or `include_ignored=true` only when the task specifically needs those normally skipped paths.
- Set `include_sensitive=true` only when the task explicitly needs files that look like secrets, credentials, keys, tokens, or private config.

Narrow first:
- Common dependency/cache directories such as node_modules, .venv, target, and build outputs are summarized from workspace-root listings by default.
- Large results are truncated; use the returned `truncated`, `returned_count`, `max_results`, `remaining_count`, and `ignored_roots` fields to choose a smaller follow-up query.
- When a backend scan limit is reached, the response can include `count_is_estimate=true`.
- Use `offset` for pagination and `sort` to choose `modified_desc` or `path_asc`.

Returns:
- A structured list of normalized file paths plus counts, truncation metadata, ignored root summaries, and scan-limit hints.
- Errors for invalid paths or paths outside the permitted workspace."#;

pub(in crate::tools::schemas) fn find_files_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "find_files",
            "description": FIND_FILES_DESCRIPTION,
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Optional search root path. Use workspace-relative path by default; absolute path is allowed when outside-workspace access is enabled. Default '.'."},
                    "glob": {"type": "string", "description": "Optional glob filter such as `**/*.rs` or `src/**/*.md`. Use it to narrow by filename, directory, or extensions before listing broad trees. Default **/*."},
                    "include_hidden": {"type": "boolean", "description": "Whether hidden files and dotfiles are included. Default false; set true only when the task explicitly needs paths such as .env.example, .github, or other hidden project files."},
                    "include_ignored": {"type": "boolean", "description": "When listing workspace root, include files under common dependency/cache directories. Default false; set true only when explicitly inspecting generated, dependency, cache, or build-output paths."},
                    "include_sensitive": {"type": "boolean", "description": "Include files whose paths look like secrets, credentials, keys, tokens, or private config. Default false."},
                    "sort": {"type": "string", "enum": ["modified_desc", "path_asc"], "description": "Sort order. `modified_desc` uses local file modification time when available; non-local backends may fall back to `path_asc`. Default modified_desc."},
                    "offset": {"type": "integer", "minimum": 0, "description": "Number of matching file paths to skip before returning results. Default 0."},
                    "max_results": {"type": "integer", "description": "Maximum number of file paths returned in one call. Default 100; larger values are capped. If truncated, use returned counts to run a narrower follow-up query."},
                    "scan_limit": {"type": "integer", "description": "Maximum files scanned before stopping early to keep listing fast. If reached, response includes `count_is_estimate=true`."}
                },
                "required": []
            }
        }
    })
}
