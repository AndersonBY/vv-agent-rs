use serde_json::{json, Value};

const FIND_FILES_DESCRIPTION: &str = "List workspace files with path, glob, visibility, pagination, and sorting controls. Results are bounded and report truncation/count hints; paths remain subject to workspace policy.";

pub(in crate::tools::schemas) fn find_files_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "find_files",
            "description": FIND_FILES_DESCRIPTION,
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Optional search root path."},
                    "glob": {"type": "string", "description": "Optional glob filter such as `**/*.py`."},
                    "include_hidden": {"type": "boolean", "description": "Whether hidden files and dotfiles are included."},
                    "include_ignored": {"type": "boolean", "description": "When listing workspace root, include files under common dependency/cache directories."},
                    "include_sensitive": {"type": "boolean", "description": "Include files whose paths look like secrets, credentials, keys, tokens, or private config."},
                    "sort": {"type": "string", "enum": ["modified_desc", "path_asc"], "description": "Sort order."},
                    "offset": {"type": "integer", "minimum": 0, "description": "Number of matching file paths to skip before returning results."},
                    "max_results": {"type": "integer", "description": "Maximum number of file paths returned in one call."},
                    "scan_limit": {"type": "integer", "description": "Maximum files scanned before stopping early to keep listing fast."}
                },
                "required": []
            }
        }
    })
}
