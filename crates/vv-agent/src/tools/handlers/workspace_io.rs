use std::sync::Arc;

use serde_json::{json, Value};

use crate::tools::base::ToolSpec;
use crate::tools::common::path_escapes_workspace_error;
use crate::tools::common::{
    collect_ignored_roots, is_hidden_path, is_ignored_root, replace_n, tool_error,
};

const READ_FILE_MAX_LINES: usize = 2_000;
const READ_FILE_MAX_CHARS: usize = 50_000;

pub(crate) fn list_files_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "list_files",
        "List files in the current workspace.",
        Arc::new(|context, arguments| {
            let path = arguments.get("path").and_then(Value::as_str).unwrap_or(".");
            let glob = arguments
                .get("glob")
                .and_then(Value::as_str)
                .unwrap_or("**/*");
            let max_results = arguments
                .get("max_results")
                .and_then(Value::as_u64)
                .unwrap_or(500)
                .clamp(1, 5_000) as usize;
            let scan_limit = arguments
                .get("scan_limit")
                .and_then(Value::as_u64)
                .unwrap_or(50_000)
                .max(max_results as u64) as usize;
            let include_ignored = arguments
                .get("include_ignored")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let include_hidden = arguments
                .get("include_hidden")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if let Err(error) = context.resolve_workspace_path(path) {
                return path_escapes_workspace_error(error);
            }
            let backend = context.effective_workspace_backend();
            match backend.list_files(path, glob) {
                Ok(mut files) => {
                    let ignored_roots = if include_ignored || path != "." {
                        Vec::new()
                    } else {
                        collect_ignored_roots(&files)
                    };
                    if !include_ignored && path == "." {
                        files.retain(|path| {
                            path.split('/')
                                .next()
                                .is_none_or(|root| !is_ignored_root(root))
                        });
                    }
                    if !include_hidden {
                        files.retain(|path| !is_hidden_path(path));
                    }
                    let actual_count = files.len();
                    let scan_limited = actual_count > scan_limit;
                    let count = if scan_limited {
                        scan_limit
                    } else {
                        actual_count
                    };
                    let returned = files.into_iter().take(max_results).collect::<Vec<_>>();
                    let mut payload = json!({
                        "files": returned,
                        "count": count,
                        "returned_count": count.min(max_results),
                        "truncated": scan_limited || count > max_results,
                        "max_results": max_results,
                    });
                    if count > max_results {
                        payload["remaining_count"] = Value::Number((count - max_results).into());
                    }
                    if scan_limited {
                        payload["count_is_estimate"] = Value::Bool(true);
                        payload["scan_limit"] = Value::Number(scan_limit.into());
                        payload["message"] = Value::String(
                            "Listing stopped early due to scan limit. Narrow `path`/`glob` or increase `scan_limit` for more complete results."
                                .to_string(),
                        );
                    }
                    if !ignored_roots.is_empty() {
                        payload["ignored_roots"] = Value::Array(
                            ignored_roots
                                .into_iter()
                                .map(|path| json!({"path": path}))
                                .collect(),
                        );
                        payload["message"] = Value::String(
                            "Common dependency/cache directories are summarized by default."
                                .to_string(),
                        );
                    }
                    crate::types::ToolExecutionResult::success("", payload.to_string())
                }
                Err(error) => tool_error(error.to_string()),
            }
        }),
    );
    if let Some(schema) = super::super::schemas::schema_for("list_files") {
        spec.schema = schema;
    }
    spec
}

pub(crate) fn file_info_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "file_info",
        "Return metadata for a workspace path.",
        Arc::new(|context, arguments| {
            let Some(path) = arguments.get("path").and_then(Value::as_str) else {
                return tool_error("missing required argument: path");
            };
            if let Err(error) = context.resolve_workspace_path(path) {
                return path_escapes_workspace_error(error);
            }
            let backend = context.effective_workspace_backend();
            match backend.file_info(path) {
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
                Err(error) => tool_error(error.to_string()),
            }
        }),
    );
    if let Some(schema) = super::super::schemas::schema_for("file_info") {
        spec.schema = schema;
    }
    spec
}

pub(crate) fn read_file_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "read_file",
        "Read a text file from the current workspace.",
        Arc::new(|context, arguments| {
            let Some(path) = arguments.get("path").and_then(Value::as_str) else {
                return tool_error("missing required argument: path");
            };
            if let Err(error) = context.resolve_workspace_path(path) {
                return path_escapes_workspace_error(error);
            }
            let backend = context.effective_workspace_backend();
            if !backend.is_file(path) {
                return tool_error(format!("file not found: {path}"));
            }
            let start_line = arguments
                .get("start_line")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .max(1) as usize;
            let end_line = arguments
                .get("end_line")
                .and_then(Value::as_u64)
                .map(|line| line.max(start_line as u64) as usize);
            let show_line_numbers = arguments
                .get("show_line_numbers")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            match backend.read_text(path) {
                Ok(text) => {
                    let lines = text.lines().collect::<Vec<_>>();
                    let start_index = start_line.saturating_sub(1).min(lines.len());
                    let end_index = end_line.unwrap_or(lines.len()).min(lines.len());
                    let selected = &lines[start_index..end_index];
                    let selected_line_count = selected.len();
                    let actual_start_line = start_index + 1;
                    let actual_end_line = start_index + selected_line_count;
                    let content = selected
                        .iter()
                        .enumerate()
                        .map(|(offset, line)| {
                            if show_line_numbers {
                                format!("{}: {line}", start_index + offset + 1)
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
                Err(error) => tool_error(error.to_string()),
            }
        }),
    );
    if let Some(schema) = super::super::schemas::schema_for("read_file") {
        spec.schema = schema;
    }
    spec
}

pub(crate) fn write_file_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "write_file",
        "Write a text file in the current workspace.",
        Arc::new(|context, arguments| {
            let Some(path) = arguments.get("path").and_then(Value::as_str) else {
                return tool_error("missing required argument: path");
            };
            if let Err(error) = context.resolve_workspace_path(path) {
                return path_escapes_workspace_error(error);
            }
            let backend = context.effective_workspace_backend();
            let content = arguments
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let append = arguments
                .get("append")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let leading_newline = append
                && arguments
                    .get("leading_newline")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
            let trailing_newline = append
                && arguments
                    .get("trailing_newline")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
            let write_content = format!(
                "{}{}{}",
                if leading_newline { "\n" } else { "" },
                content,
                if trailing_newline { "\n" } else { "" }
            );
            match backend.write_text(path, &write_content, append) {
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
                Err(error) => tool_error(error.to_string()),
            }
        }),
    );
    if let Some(schema) = super::super::schemas::schema_for("write_file") {
        spec.schema = schema;
    }
    spec
}

pub(crate) fn file_str_replace_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "file_str_replace",
        "Replace text in a workspace file.",
        Arc::new(|context, arguments| {
            let Some(path) = arguments.get("path").and_then(Value::as_str) else {
                return tool_error("missing required argument: path");
            };
            if let Err(error) = context.resolve_workspace_path(path) {
                return path_escapes_workspace_error(error);
            }
            let backend = context.effective_workspace_backend();
            if !backend.is_file(path) {
                return tool_error(format!("file not found: {path}"));
            }
            let Some(old_str) = arguments.get("old_str").and_then(Value::as_str) else {
                return tool_error("missing required argument: old_str");
            };
            if old_str.is_empty() {
                return tool_error("`old_str` cannot be empty");
            }
            let new_str = arguments
                .get("new_str")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let replace_all = arguments
                .get("replace_all")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let max_replacements = arguments
                .get("max_replacements")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .max(1) as usize;
            match backend.read_text(path) {
                Ok(text) => {
                    let occurrence_count = text.matches(old_str).count();
                    if occurrence_count == 0 {
                        return tool_error("`old_str` not found in file");
                    }
                    let replaced_count = if replace_all {
                        occurrence_count
                    } else {
                        occurrence_count.min(max_replacements)
                    };
                    let replaced_text = if replace_all {
                        text.replace(old_str, new_str)
                    } else {
                        replace_n(&text, old_str, new_str, max_replacements)
                    };
                    match backend.write_text(path, &replaced_text, false) {
                        Ok(_) => crate::types::ToolExecutionResult::success(
                            "",
                            json!({
                                "ok": true,
                                "path": path,
                                "replaced_count": replaced_count,
                            })
                            .to_string(),
                        ),
                        Err(error) => tool_error(error.to_string()),
                    }
                }
                Err(error) => tool_error(error.to_string()),
            }
        }),
    );
    if let Some(schema) = super::super::schemas::schema_for("file_str_replace") {
        spec.schema = schema;
    }
    spec
}
