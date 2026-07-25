use serde_json::{json, Value};

const SEARCH_FILES_DESCRIPTION: &str = "Search workspace text with regex and smart-case by default; set literal for exact text. Scope with path, glob, or type. Results are bounded and return truncation/pagination metadata.";

pub(in crate::tools::schemas) fn search_files_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "search_files",
            "description": SEARCH_FILES_DESCRIPTION,
            "parameters": {
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Regex pattern, or exact text when literal is true."},
                    "path": {"type": "string", "description": "Optional search root or single file path."},
                    "glob": {"type": "string", "description": "Optional file glob filter such as `**/*.rs`."},
                    "include_hidden": {"type": "boolean", "description": "Whether hidden files and dotfiles are included."},
                    "include_ignored": {"type": "boolean", "description": "When searching workspace root, include files under common dependency/cache directories."},
                    "include_sensitive": {"type": "boolean", "description": "Include files whose paths look like secrets, credentials, keys, tokens, or private config."},
                    "output_mode": {"type": "string", "enum": ["files_with_matches", "content", "count"], "description": "files_with_matches, content, or count."},
                    "literal": {"type": "boolean", "description": "Search for the exact pattern text instead of interpreting it as a regex."},
                    "b": {"type": "integer", "description": "Lines before each match."},
                    "a": {"type": "integer", "description": "Lines after each match."},
                    "c": {"type": "integer", "description": "Context lines before and after each match."},
                    "n": {"type": "boolean", "description": "Whether to include line numbers in content output."},
                    "type": {"type": "string", "description": "File type shortcut such as py, js, ts, md, or json."},
                    "offset": {"type": "integer", "minimum": 0, "description": "Number of result rows or entries to skip before returning results."},
                    "head_limit": {"type": "integer", "minimum": 0, "description": "Cap output to the first N rows or entries."},
                    "multiline": {"type": "boolean", "description": "Enable multiline regex mode."},
                    "case_sensitive": {"type": "boolean", "description": "Explicitly override smart-case behavior."}
                },
                "required": ["pattern"]
            }
        }
    })
}
