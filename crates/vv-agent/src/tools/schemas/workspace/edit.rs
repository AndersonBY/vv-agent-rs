use serde_json::{json, Value};

const FILE_STR_REPLACE_DESCRIPTION: &str = r#"Replace text in a workspace file.

Use exact `old_str` matching.

Workflow:
- Call `read_file` first so you can copy the exact surrounding text, including indentation and line endings.
- Use this for focused edits where a precise old/new string is safer than rewriting a whole file.
- Include enough context in `old_str` to make the target unique; never guess whitespace or punctuation from memory.
- The operation fails if `old_str` is not found, which protects against stale assumptions.
- By default only one match is replaced; use `replace_all=true` or `max_replacements` intentionally after confirming scope.

Returns:
- Structured edit metadata including normalized path, replacements made, and whether the file changed.
- Clear failure details when the target string is missing, ambiguous, outside the workspace, or rejected by the backend.

Prefer this over shell-based sed/perl for repository edits because it is exact, structured, and safer for Agent-driven changes."#;

pub(in crate::tools::schemas) fn file_str_replace_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "file_str_replace",
            "description": FILE_STR_REPLACE_DESCRIPTION,
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Target file path (workspace-relative by default; absolute path allowed when outside-workspace access is enabled)."},
                    "old_str": {"type": "string", "description": "The source text to replace. This must be the exact source text, with enough context to make the match unique."},
                    "new_str": {"type": "string", "description": "Replacement text. Preserve intended indentation, line endings, surrounding whitespace, and unchanged context from the `read_file` snippet."},
                    "replace_all": {"type": "boolean", "description": "Replace all matches when true. Default false."},
                    "max_replacements": {"type": "integer", "minimum": 1, "description": "Optional cap when replace_all=false. Default 1."}
                },
                "required": ["path", "old_str", "new_str"]
            }
        }
    })
}
