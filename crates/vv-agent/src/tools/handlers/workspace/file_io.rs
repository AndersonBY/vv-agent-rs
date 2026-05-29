use std::sync::Arc;

use serde_json::{json, Value};

use crate::tools::base::{ToolContext, ToolSpec};
use crate::tools::common::{
    coerce_truthy_arg, parse_integer_arg, path_escapes_workspace_error, stringify_tool_arg,
    tool_error,
};
use crate::types::{ToolArguments, ToolExecutionResult};

use super::workspace_backend_error;

const READ_FILE_MAX_LINES: usize = 2_000;
const READ_FILE_MAX_CHARS: usize = 50_000;

pub fn file_info(context: &mut ToolContext, arguments: &ToolArguments) -> ToolExecutionResult {
    let spec = file_info_tool();
    (spec.handler)(context, arguments)
}

pub(crate) fn file_info_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "file_info",
        "Return metadata for a workspace path.",
        Arc::new(|context, arguments| {
            if !arguments.contains_key("path") {
                return tool_error("missing required argument: path");
            }
            let path = stringify_tool_arg(arguments.get("path"), "");
            if let Err(error) = context.resolve_workspace_path(&path) {
                return path_escapes_workspace_error(error);
            }
            let backend = context.effective_workspace_backend();
            match backend.file_info(&path) {
                Ok(Some(info)) => {
                    let mut payload = json!({
                        "path": info.path,
                        "exists": true,
                        "is_file": info.is_file,
                        "is_dir": info.is_dir,
                        "size": info.size,
                        "modified_at": info.modified_at,
                    });
                    if info.is_file {
                        payload["suffix"] = Value::String(info.suffix);
                    }
                    crate::types::ToolExecutionResult::success("", payload.to_string())
                }
                Ok(None) => tool_error(format!("path not found: {path}")),
                Err(error) => workspace_backend_error(error),
            }
        }),
    );
    if let Some(schema) = crate::tools::schemas::schema_for("file_info") {
        spec.schema = schema;
    }
    spec
}

pub fn read_file(context: &mut ToolContext, arguments: &ToolArguments) -> ToolExecutionResult {
    let spec = read_file_tool();
    (spec.handler)(context, arguments)
}

pub(crate) fn read_file_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "read_file",
        "Read a text file from the current workspace.",
        Arc::new(|context, arguments| {
            if !arguments.contains_key("path") {
                return tool_error("missing required argument: path");
            }
            let path = stringify_tool_arg(arguments.get("path"), "");
            if let Err(error) = context.resolve_workspace_path(&path) {
                return path_escapes_workspace_error(error);
            }
            let backend = context.effective_workspace_backend();
            match backend.file_info(&path) {
                Ok(Some(info)) if info.is_file => {}
                Ok(_) => return tool_error(format!("file not found: {path}")),
                Err(error) => return workspace_backend_error(error),
            }
            let start_line = match arguments.get("start_line") {
                Some(value) => match parse_integer_arg(value) {
                    Ok(line) => line.max(1) as usize,
                    Err(_) => return tool_error("`start_line`/`end_line` must be integers"),
                },
                None => 1,
            };
            let end_line = match arguments.get("end_line") {
                Some(value) => match parse_integer_arg(value) {
                    Ok(line) => Some(line.max(start_line as i64) as usize),
                    Err(_) => return tool_error("`start_line`/`end_line` must be integers"),
                },
                None => None,
            };
            let show_line_numbers = coerce_truthy_arg(arguments.get("show_line_numbers"), false);
            match backend.read_text(&path) {
                Ok(text) => {
                    let lines = text.lines().collect::<Vec<_>>();
                    let requested_start_index = start_line.saturating_sub(1);
                    let slice_start_index = requested_start_index.min(lines.len());
                    let slice_end_index = end_line
                        .unwrap_or(lines.len())
                        .min(lines.len())
                        .max(slice_start_index);
                    let selected = &lines[slice_start_index..slice_end_index];
                    let selected_line_count = selected.len();
                    let actual_start_line = requested_start_index + 1;
                    let actual_end_line = requested_start_index + selected_line_count;
                    let content = selected
                        .iter()
                        .enumerate()
                        .map(|(offset, line)| {
                            if show_line_numbers {
                                format!("{}: {line}", actual_start_line + offset)
                            } else {
                                (*line).to_string()
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    if selected_line_count > READ_FILE_MAX_LINES
                        || content.len() > READ_FILE_MAX_CHARS
                    {
                        let total_lines = lines.len();
                        let total_chars = text.len();
                        let suggested_start = start_line.min(total_lines.max(1));
                        let suggested_end =
                            (suggested_start + READ_FILE_MAX_LINES - 1).min(total_lines);
                        return crate::types::ToolExecutionResult::success(
                            "",
                            json!({
                                "path": path,
                                "start_line": actual_start_line,
                                "end_line": actual_end_line,
                                "show_line_numbers": show_line_numbers,
                                "content": Value::Null,
                                "file_info": {
                                    "total_lines": total_lines,
                                    "total_chars": total_chars,
                                },
                                "requested": {
                                    "line_count": selected_line_count,
                                    "char_count": content.len(),
                                },
                                "limits": {
                                    "max_lines": READ_FILE_MAX_LINES,
                                    "max_chars": READ_FILE_MAX_CHARS,
                                },
                                "suggested_range": {
                                    "start_line": suggested_start,
                                    "end_line": suggested_end,
                                },
                                "message": "Requested read exceeds limits. Use start_line/end_line for a smaller range.",
                            })
                            .to_string(),
                        );
                    }
                    crate::types::ToolExecutionResult::success(
                        "",
                        json!({
                            "path": path,
                            "start_line": actual_start_line,
                            "end_line": actual_end_line,
                            "show_line_numbers": show_line_numbers,
                            "content": content,
                        })
                        .to_string(),
                    )
                }
                Err(error) => workspace_backend_error(error),
            }
        }),
    );
    if let Some(schema) = crate::tools::schemas::schema_for("read_file") {
        spec.schema = schema;
    }
    spec
}

pub fn write_file(context: &mut ToolContext, arguments: &ToolArguments) -> ToolExecutionResult {
    let spec = write_file_tool();
    (spec.handler)(context, arguments)
}

pub(crate) fn write_file_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "write_file",
        "Write a text file in the current workspace.",
        Arc::new(|context, arguments| {
            if !arguments.contains_key("path") {
                return tool_error("missing required argument: path");
            }
            let path = stringify_tool_arg(arguments.get("path"), "");
            if let Err(error) = context.resolve_workspace_path(&path) {
                return path_escapes_workspace_error(error);
            }
            let backend = context.effective_workspace_backend();
            let content = stringify_tool_arg(arguments.get("content"), "");
            let append = coerce_truthy_arg(arguments.get("append"), false);
            let leading_newline =
                append && coerce_truthy_arg(arguments.get("leading_newline"), false);
            let trailing_newline =
                append && coerce_truthy_arg(arguments.get("trailing_newline"), false);
            let write_content = format!(
                "{}{}{}",
                if leading_newline { "\n" } else { "" },
                content.as_str(),
                if trailing_newline { "\n" } else { "" }
            );
            match backend.write_text(&path, &write_content, append) {
                Ok(written) => crate::types::ToolExecutionResult::success(
                    "",
                    json!({
                        "ok": true,
                        "path": path,
                        "append": append,
                        "leading_newline": leading_newline,
                        "trailing_newline": trailing_newline,
                        "written_chars": written,
                    })
                    .to_string(),
                ),
                Err(error) => workspace_backend_error(error),
            }
        }),
    );
    if let Some(schema) = crate::tools::schemas::schema_for("write_file") {
        spec.schema = schema;
    }
    spec
}
