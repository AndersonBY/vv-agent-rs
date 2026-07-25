use std::sync::Arc;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::tools::base::{ToolContext, ToolSpec};
use crate::tools::common::{bool_arg, integer_arg, path_escapes_workspace_error, string_arg};
use crate::types::{
    Metadata, ToolArguments, ToolExecutionResult, ToolResultCursor, ToolTruncationReason,
};

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
            let cursor = match arguments.get("cursor") {
                None => None,
                Some(Value::Object(_)) => {
                    match serde_json::from_value::<ToolResultCursor>(arguments["cursor"].clone()) {
                        Ok(cursor) if cursor.validate().is_ok() => Some(cursor),
                        _ => {
                            return workspace_tool_error(
                                "`cursor` is invalid",
                                "invalid_arguments",
                                &path,
                            )
                        }
                    }
                }
                Some(_) => {
                    return workspace_tool_error(
                        "`cursor` must be an object",
                        "invalid_arguments",
                        &path,
                    )
                }
            };
            if cursor.is_some()
                && (arguments.contains_key("start_line") || arguments.contains_key("end_line"))
            {
                return workspace_tool_error(
                    "`cursor` is incompatible with `start_line` and `end_line`",
                    "invalid_arguments",
                    &path,
                );
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
                    let digest = sha256_hex(&raw);
                    let normalized_path = normalize_cursor_path(&path);
                    let start_offset = match cursor.as_ref() {
                        Some(cursor) if normalize_cursor_path(&cursor.path) != normalized_path => {
                            return workspace_tool_error(
                                "cursor path does not match requested path",
                                "cursor_path_mismatch",
                                &path,
                            )
                        }
                        Some(cursor) if cursor.sha256 != digest => {
                            return workspace_tool_error(
                                "source changed after cursor was issued",
                                "stale_cursor",
                                &path,
                            )
                        }
                        Some(cursor) => match usize::try_from(cursor.offset_chars) {
                            Ok(offset) if offset <= text.chars().count() => offset,
                            _ => {
                                return workspace_tool_error(
                                    "cursor offset is outside the source",
                                    "cursor_offset_invalid",
                                    &path,
                                )
                            }
                        },
                        None => line_start_offset(&text, start_line),
                    };
                    let end_offset = if cursor.is_some() {
                        text.chars().count()
                    } else {
                        line_end_offset(&text, end_line).max(start_offset)
                    };
                    let slice =
                        bounded_source_slice(&text, start_offset, end_offset, show_line_numbers);
                    record_file_baseline(
                        context,
                        &path,
                        &raw,
                        is_partial_request || cursor.is_some() || slice.next_offset < end_offset,
                        READ_FILE_BASELINE_SOURCE,
                    );
                    read_text_result(&path, digest, slice)
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

fn read_text_result(path: &str, sha256: String, slice: BoundedSourceSlice) -> ToolExecutionResult {
    let mut result = ToolExecutionResult::success("", slice.content);
    if slice.next_offset < slice.end_offset {
        result.truncated = true;
        result.truncation_reason = Some(ToolTruncationReason::ReadLimit);
        result.original_bytes = Some(slice.original_bytes);
        result.visible_bytes = Some(result.content.len() as u64);
        result.cursor = Some(ToolResultCursor {
            kind: "read_file".to_string(),
            path: normalize_cursor_path(path),
            offset_chars: slice.next_offset as u64,
            sha256,
        });
    }
    result
}

struct BoundedSourceSlice {
    content: String,
    next_offset: usize,
    end_offset: usize,
    original_bytes: u64,
}

fn bounded_source_slice(
    text: &str,
    start_offset: usize,
    end_offset: usize,
    show_line_numbers: bool,
) -> BoundedSourceSlice {
    let prefix_text = text.chars().take(start_offset).collect::<String>();
    let start_line = prefix_text
        .chars()
        .filter(|character| *character == '\n')
        .count()
        + 1;
    let mut output = String::new();
    let mut visible_chars = 0usize;
    let mut consumed = 0usize;
    let mut output_line_count = 0usize;
    let mut source_line_count = 0usize;
    let mut original_bytes = 0u64;
    let mut output_full = false;
    let mut at_line_start = start_offset == 0 || prefix_text.ends_with('\n');
    for character in text
        .chars()
        .skip(start_offset)
        .take(end_offset.saturating_sub(start_offset))
    {
        let prefix = if show_line_numbers && at_line_start {
            format!("{}: ", start_line + source_line_count)
        } else {
            String::new()
        };
        original_bytes += (prefix.len() + character.len_utf8()) as u64;
        if !output_full {
            let added_chars = prefix.chars().count() + 1;
            if visible_chars + added_chars > READ_FILE_MAX_CHARS
                || (at_line_start && output_line_count >= READ_FILE_MAX_LINES)
            {
                output_full = true;
            } else {
                output.push_str(&prefix);
                output.push(character);
                visible_chars += added_chars;
                consumed += 1;
                if character == '\n' {
                    output_line_count += 1;
                }
            }
        }
        at_line_start = character == '\n';
        if at_line_start {
            source_line_count += 1;
        }
    }
    BoundedSourceSlice {
        content: output,
        next_offset: start_offset + consumed,
        end_offset,
        original_bytes,
    }
}

fn line_start_offset(text: &str, start_line: usize) -> usize {
    if start_line <= 1 {
        return 0;
    }
    let mut lines_seen = 1usize;
    for (offset, character) in text.chars().enumerate() {
        if character == '\n' {
            lines_seen += 1;
            if lines_seen == start_line {
                return offset + 1;
            }
        }
    }
    text.chars().count()
}

fn line_end_offset(text: &str, end_line: Option<usize>) -> usize {
    let Some(end_line) = end_line else {
        return text.chars().count();
    };
    let mut lines_seen = 1usize;
    for (offset, character) in text.chars().enumerate() {
        if character == '\n' {
            if lines_seen == end_line {
                return offset;
            }
            lines_seen += 1;
        }
    }
    text.chars().count()
}

fn normalize_cursor_path(path: &str) -> String {
    let mut normalized = path.trim().replace('\\', "/");
    while let Some(stripped) = normalized.strip_prefix("./") {
        normalized = stripped.to_string();
    }
    normalized
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
