use serde_json::{json, Value};

const EDIT_FILE_DESCRIPTION: &str = "Replace exact text in a workspace file using the current read/write baseline. The match must be unique unless replace_all is true, and changed files or stale baselines are rejected.";

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
                    "old_string": {"type": "string", "description": "Exact source text to replace."},
                    "new_string": {"type": "string", "description": "Replacement text."},
                    "replace_all": {"type": "boolean", "description": "Replace all matches when true after confirming every match is intended."}
                },
                "required": ["path", "old_string", "new_string"]
            }
        }
    })
}
