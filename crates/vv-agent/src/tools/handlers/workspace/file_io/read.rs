use std::sync::Arc;

use serde_json::{json, Value};

use crate::tools::base::{ToolContext, ToolSpec};
use crate::tools::common::{bool_arg, integer_arg, path_escapes_workspace_error, string_arg};
use crate::types::{Metadata, ToolArguments, ToolExecutionResult};

use super::super::edit::{
    decode_workspace_text, record_file_baseline, workspace_tool_error,
    workspace_tool_error_with_details, READ_FILE_BASELINE_SOURCE,
};
use super::super::workspace_backend_error;
use super::{READ_FILE_MAX_CHARS, READ_FILE_MAX_LINES};

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
                return workspace_tool_error_with_details(
                    "`path` is required.",
                    "invalid_arguments",
                    Metadata::from([("missing_arguments".to_string(), json!(["path"]))]),
                );
            }
            let path = string_arg(arguments.get("path"), "");
            if let Err(error) = context.resolve_workspace_path(&path) {
                return path_escapes_workspace_error(error);
            }
            let backend = context.effective_workspace_backend();
            match backend.file_info(&path) {
                Ok(Some(info)) if info.is_file => {}
                Ok(_) => {
                    return workspace_tool_error(
                        format!("file not found: {path}"),
                        "file_not_found",
                        &path,
                    )
                }
                Err(error) => return workspace_backend_error(error),
            }
            let start_line = match arguments.get("start_line") {
                Some(value) => match integer_arg(value) {
                    Ok(line) => line.max(1) as usize,
                    Err(_) => {
                        return workspace_tool_error(
                            "`start_line`/`end_line` must be integers",
                            "invalid_arguments",
                            &path,
                        )
                    }
                },
                None => 1,
            };
            let end_line = match arguments.get("end_line") {
                Some(value) => match integer_arg(value) {
                    Ok(line) => Some(line.max(start_line as i64) as usize),
                    Err(_) => {
                        return workspace_tool_error(
                            "`start_line`/`end_line` must be integers",
                            "invalid_arguments",
                            &path,
                        )
                    }
                },
                None => None,
            };
            let show_line_numbers = bool_arg(arguments.get("show_line_numbers"), false);
            match backend.read_bytes(&path) {
                Ok(raw) => {
                    let text = match decode_workspace_text(&raw) {
                        Ok((text, _has_bom)) => text,
                        Err(_) => {
                            return workspace_tool_error(
                                "Unsupported file encoding for read_file.",
                                "unsupported_encoding",
                                &path,
                            )
                        }
                    };
                    let is_partial_request = start_line != 1 || end_line.is_some();
                    let selection = read_selection(&text, start_line, end_line, show_line_numbers);
                    let is_oversized = selection.selected_line_count > READ_FILE_MAX_LINES
                        || selection.selected_char_count > READ_FILE_MAX_CHARS;
                    record_file_baseline(
                        context,
                        &path,
                        &raw,
                        is_partial_request || is_oversized,
                        READ_FILE_BASELINE_SOURCE,
                    );
                    read_text_result(&path, &text, selection)
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

fn read_selection(
    text: &str,
    start_line: usize,
    end_line: Option<usize>,
    show_line_numbers: bool,
) -> ReadSelection {
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
    ReadSelection {
        start_line,
        actual_start_line,
        actual_end_line,
        selected_line_count,
        selected_char_count: content.chars().count(),
        show_line_numbers,
        content,
    }
}

fn read_text_result(path: &str, text: &str, selection: ReadSelection) -> ToolExecutionResult {
    if selection.selected_line_count > READ_FILE_MAX_LINES
        || selection.selected_char_count > READ_FILE_MAX_CHARS
    {
        return oversized_read_result(path, text, selection);
    }
    ToolExecutionResult::success(
        "",
        json!({
            "path": path,
            "start_line": selection.actual_start_line,
            "end_line": selection.actual_end_line,
            "show_line_numbers": selection.show_line_numbers,
            "content": selection.content,
        })
        .to_string(),
    )
}

struct ReadSelection {
    start_line: usize,
    actual_start_line: usize,
    actual_end_line: usize,
    selected_line_count: usize,
    selected_char_count: usize,
    show_line_numbers: bool,
    content: String,
}

fn oversized_read_result(path: &str, text: &str, selection: ReadSelection) -> ToolExecutionResult {
    let total_lines = text.lines().count();
    let total_chars = text.chars().count();
    let suggested_start = selection.start_line.min(total_lines.max(1));
    let suggested_end = (suggested_start + READ_FILE_MAX_LINES - 1).min(total_lines);
    ToolExecutionResult::success(
        "",
        json!({
            "path": path,
            "start_line": selection.actual_start_line,
            "end_line": selection.actual_end_line,
            "show_line_numbers": selection.show_line_numbers,
            "content": Value::Null,
            "file_info": {
                "total_lines": total_lines,
                "total_chars": total_chars,
            },
            "requested": {
                "line_count": selection.selected_line_count,
                "char_count": selection.selected_char_count,
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
    )
}
