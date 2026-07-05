use serde_json::{json, Value};

const EDIT_FILE_DESCRIPTION: &str = r#"Safely edit an existing workspace file by replacing exact text.

Use exact `old_string` matching.

Workflow:
- Call `read_file` for the full file first so the runtime can verify the file was not changed before editing.
- Use this for focused edits where a precise old/new string is safer than rewriting a whole file.
- Include enough context in `old_string` to make the target unique; never guess whitespace or punctuation from memory.
- The operation fails if `old_string` is not found, if it matches multiple locations, or if the file changed since it was read.
- By default `old_string` must match exactly one location; use `replace_all=true` only after confirming every match should change.

Returns:
- Short JSON content with replacement count.
- Structured edit metadata including changed files, bounded diff, additions, deletions, operation, and line ending.
- Clear failure details when the target string is missing, ambiguous, outside the workspace, or rejected by the backend.

Prefer this over shell-based sed/perl for repository edits because it is exact, structured, and safer for Agent-driven changes."#;

pub(in crate::tools::schemas) fn edit_file_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "edit_file",
            "description": EDIT_FILE_DESCRIPTION,
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Target file path (workspace-relative by default; absolute path allowed when outside-workspace access is enabled)."},
                    "old_string": {"type": "string", "description": "Exact source text to replace. Must be non-empty and unique unless replace_all=true."},
                    "new_string": {"type": "string", "description": "Replacement text. May be empty; preserve intended indentation, line endings, and surrounding whitespace."},
                    "replace_all": {"type": "boolean", "description": "Replace all matches when true after confirming every match is intended. Default false to keep focused edits narrow."}
                },
                "required": ["path", "old_string", "new_string"]
            }
        }
    })
}
