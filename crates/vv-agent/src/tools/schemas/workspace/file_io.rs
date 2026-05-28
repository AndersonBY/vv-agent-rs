use serde_json::{json, Value};

const READ_FILE_DESCRIPTION: &str = r#"Read file contents from workspace.

When to use:
- Inspect source files, configs, logs, docs, generated artifacts, or exact snippets without shelling out to cat/head/tail.
- Read large files in chunks after using `file_info` or after a truncated response suggests a narrower line range.
- Use `show_line_numbers=true` when you need to quote lines, plan precise edits, or coordinate with `file_str_replace`.

Supported behavior:
- Reads plain UTF-8 text files and returns a content slice.
- Uses 1-based line numbers for `start_line` and `end_line`.
- Can prepend line numbers with `show_line_numbers=true`.
- Enforces read limits per request: max 2000 lines or 50000 characters.
- Large reads return file info payload instead of full content.

Guidance:
- Prefer this tool instead of shell commands like cat/head/tail.
- For large files, read in chunks by line range.
- By default, paths are workspace-relative.
- If runtime metadata enables outside-workspace access, absolute local paths are allowed.

Returns:
- A UTF-8 text slice with path metadata, requested line range, actual returned range, and optional line numbers.
- If the request exceeds safe limits, a file-info style payload with file statistics and suggested smaller ranges instead of flooding the LLM context.

Safety and limits:
- Uses 1-based inclusive line numbers for `start_line` and `end_line`.
- Enforces max 2000 lines or 50000 characters per request.
- Prefer `file_info` before reading unknown large or binary-looking paths.
- Paths are workspace-relative by default; absolute local paths require explicit outside-workspace runtime permission."#;

const WRITE_FILE_DESCRIPTION: &str = r#"Write content to a file in workspace.

MODES:
- Overwrite (default): Replaces entire file content.
- Append: Adds to existing content (`append=true`).

WARNING:
- By default, this OVERWRITES the entire file.
- Use `append=true` to add content instead.

PARAMETERS:
- `path` (required): Workspace-relative path by default. Absolute path is allowed when outside-workspace access is enabled.
- `content` (required): Content to write.
- `append` (optional): Set true to append instead of overwrite.
- `leading_newline`/`trailing_newline` (optional): Add newlines when appending.

When to use:
- Create a new file, replace an entire generated artifact, or append a clearly bounded section to an existing file.
- Use `append=true` only when preserving all existing content is intentional and the appended block boundary is clear.
- Prefer `file_str_replace` for small or surgical edits to existing files.

Do not use this for surgical edits to existing source files; prefer `file_str_replace` after `read_file` so exact context and whitespace are preserved.

Returns:
- Structured write metadata including normalized path, append mode, character count, and newline flags.
- Errors when the path escapes the workspace or the backend refuses the write.

Safety and behavior:
- Overwrite is the default and replaces the whole file.
- This can create parent directories when the workspace backend supports it.
- Parent directories may be created when the workspace backend supports it."#;

const FILE_INFO_DESCRIPTION: &str = r#"Read file metadata in workspace, including size, modified time and type.

Inspect file metadata in workspace without loading full contents.

When to use:
- Use before reading large or binary files.
- Before reading large or binary files, before deciding read ranges, or before editing a path whose size/type is unknown.
- Check whether a path is a file or directory and whether it has a suffix that suggests text, image, archive, or binary content.
- Estimate whether `read_file`, `read_image`, or a narrower grep/search is the right next tool.

Returns:
- Normalized path, file/dir flags, byte size, modified time, suffix, and line count when it can be determined safely.
- Structured errors for missing paths or paths outside the permitted workspace.

Safety:
- This is a metadata probe; it should be preferred over reading a whole unknown file just to decide what to do next."#;

pub(in crate::tools::schemas) fn read_file_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "read_file",
            "description": READ_FILE_DESCRIPTION,
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Target file path (workspace-relative by default; absolute path allowed when outside-workspace access is enabled)."},
                    "start_line": {"type": "integer", "minimum": 1, "description": "Optional starting line number (1-based)."},
                    "end_line": {"type": "integer", "minimum": 1, "description": "Optional ending line number (1-based, inclusive)."},
                    "show_line_numbers": {"type": "boolean", "description": "When true, prefixes each output line with its source line number."}
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
