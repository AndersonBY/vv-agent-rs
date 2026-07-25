use serde_json::{json, Value};

const READ_FILE_DESCRIPTION: &str = "Read bounded UTF-8 text from a workspace path. Use a line range for targeted reads or the returned cursor to continue an oversized read; stale cursors are rejected if the source changes.";

const WRITE_FILE_DESCRIPTION: &str = "Create, overwrite, or append text in the workspace. Overwrite is the default; use append and newline controls only when preserving existing content is intentional.";

const FILE_INFO_DESCRIPTION: &str = "Inspect workspace path metadata without reading file contents. Returns normalized type, size, modified time, suffix, and line-count information when available.";

pub(in crate::tools::schemas) fn read_file_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "read_file",
            "description": READ_FILE_DESCRIPTION,
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Workspace path to read."},
                    "start_line": {"type": "integer", "minimum": 1, "description": "Optional 1-based first line; incompatible with cursor."},
                    "end_line": {"type": "integer", "minimum": 1, "description": "Optional 1-based inclusive last line; incompatible with cursor."},
                    "show_line_numbers": {"type": "boolean", "description": "Prefix returned lines with source line numbers."},
                    "cursor": {
                        "type": "object",
                        "description": "Continuation state returned by a previous read of this path.",
                        "properties": {
                            "kind": {"type": "string", "const": "read_file"},
                            "path": {"type": "string", "minLength": 1},
                            "offset_chars": {"type": "integer", "minimum": 0},
                            "sha256": {"type": "string", "pattern": "^[0-9a-f]{64}$"}
                        },
                        "required": ["kind", "offset_chars", "path", "sha256"],
                        "additionalProperties": false
                    }
                },
                "required": ["path"]
            }
        }
    })
}

pub(in crate::tools::schemas) fn write_file_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "write_file",
            "description": WRITE_FILE_DESCRIPTION,
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Target file path (workspace-relative by default; absolute path allowed when outside-workspace access is enabled)."},
                    "content": {"type": "string", "description": "The complete file body for overwrite mode, or the exact block to append when `append=true`."},
                    "append": {"type": "boolean", "description": "Set true to append instead of overwrite."},
                    "leading_newline": {"type": "boolean", "description": "Add a leading newline when appending."},
                    "trailing_newline": {"type": "boolean", "description": "Add a trailing newline when appending."}
                },
                "required": ["path", "content"]
            }
        }
    })
}

pub(in crate::tools::schemas) fn file_info_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "file_info",
            "description": FILE_INFO_DESCRIPTION,
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
