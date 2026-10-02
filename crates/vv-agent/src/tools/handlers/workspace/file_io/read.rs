use std::sync::Arc;

use serde_json::{json, Value};

use crate::tools::base::{ToolContext, ToolSpec};
use crate::tools::common::{bool_arg, integer_arg, path_escapes_workspace_error, string_arg};
use crate::types::{
    Metadata, ToolArguments, ToolExecutionResult, ToolResultCursor, ToolTruncationReason,
};

use super::super::edit::{
    workspace_tool_error, workspace_tool_error_with_details, READ_FILE_BASELINE_SOURCE,
};
use super::super::workspace_backend_error;
use super::{READ_FILE_MAX_CHARS, READ_FILE_MAX_LINES};
use crate::workspace::streaming::scan_text;

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
            let normalized_path = normalize_cursor_path(&path);
            if cursor
                .as_ref()
                .is_some_and(|cursor| normalize_cursor_path(&cursor.path) != normalized_path)
            {
                return workspace_tool_error(
                    "cursor path does not match requested path",
                    "cursor_path_mismatch",
                    &path,
                );
            }
            let requested_offset = match cursor
                .as_ref()
                .map(|cursor| usize::try_from(cursor.offset_chars))
            {
                Some(Ok(offset)) => Some(offset),
                Some(Err(_)) => {
                    return workspace_tool_error(
                        "cursor offset is outside the source",
                        "cursor_offset_invalid",
                        &path,
                    )
                }
                None => None,
            };
            let mut page = ReadPage::new(start_line, end_line, requested_offset, show_line_numbers);
            let scanned = match scan_text(backend.as_ref(), &path, |text| page.consume(text)) {
                Ok(scanned) => scanned,
                Err(error) => return workspace_backend_error(error),
            };
            if !scanned.valid_utf8 {
                return workspace_tool_error(
                    "Unsupported file encoding for read_file.",
                    "unsupported_encoding",
                    &path,
                );
            }
            if cursor
                .as_ref()
                .is_some_and(|cursor| cursor.sha256 != scanned.sha256)
            {
                return workspace_tool_error(
                    "source changed after cursor was issued",
                    "stale_cursor",
                    &path,
                );
            }
            if requested_offset.is_some_and(|offset| offset > page.offset) {
                return workspace_tool_error(
                    "cursor offset is outside the source",
                    "cursor_offset_invalid",
                    &path,
                );
            }
            let line_ending = if page.crlf > 0 && page.crlf == page.lf {
                "crlf"
            } else if page.crlf > 0 {
                "mixed"
            } else {
                "lf"
            };
            let baseline = json!({"hash": scanned.sha256, "size": scanned.size_bytes, "line_ending": line_ending,
                "is_partial": start_line != 1 || end_line.is_some() || cursor.is_some() || page.truncated,
                "source": READ_FILE_BASELINE_SOURCE});
            let baselines = context
                .shared_state
                .entry(super::super::edit::FILE_BASELINES_STATE_KEY.to_string())
                .or_insert_with(|| json!({}));
            if !baselines.is_object() {
                *baselines = json!({});
            }
            baselines
                .as_object_mut()
                .expect("baselines object")
                .insert(path.clone(), baseline);
            let slice = BoundedSourceSlice {
                content: page.output,
                next_offset: page.next_offset,
                truncated: page.truncated,
                original_bytes: page.original_bytes,
            };
            read_text_result(&path, scanned.sha256, slice)
        }),
    );
    if let Some(schema) = crate::tools::schemas::schema_for("read_file") {
        spec.schema = schema;
    }
    spec
}

fn read_text_result(path: &str, sha256: String, slice: BoundedSourceSlice) -> ToolExecutionResult {
    let mut result = ToolExecutionResult::success("", slice.content);
    if slice.truncated {
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
    truncated: bool,
    original_bytes: u64,
}

struct ReadPage {
    start_line: usize,
    end_line: Option<usize>,
    requested_offset: Option<usize>,
    show_line_numbers: bool,
    output: String,
    offset: usize,
    next_offset: usize,
    line: usize,
    at_line_start: bool,
    previous: char,
    visible_chars: usize,
    output_lines: usize,
    original_bytes: u64,
    truncated: bool,
    lf: usize,
    crlf: usize,
    first: bool,
}

impl ReadPage {
    fn new(
        start_line: usize,
        end_line: Option<usize>,
        offset: Option<usize>,
        show_line_numbers: bool,
    ) -> Self {
        Self {
            start_line,
            end_line,
            requested_offset: offset,
            show_line_numbers,
            output: String::new(),
            offset: 0,
            next_offset: offset.unwrap_or(0),
            line: 1,
            at_line_start: true,
            previous: '\0',
            visible_chars: 0,
            output_lines: 0,
            original_bytes: 0,
            truncated: false,
            lf: 0,
            crlf: 0,
            first: true,
        }
    }

    fn consume(&mut self, text: &str) {
        for character in text.chars() {
            if self.first {
                self.first = false;
                if character == '\u{feff}' {
                    continue;
                }
            }
            let selected = match self.requested_offset {
                Some(offset) => self.offset >= offset,
                None => {
                    self.line >= self.start_line
                        && self.end_line.is_none_or(|end| {
                            self.line < end || (self.line == end && character != '\n')
                        })
                }
            };
            if selected {
                let prefix = if self.show_line_numbers && self.at_line_start {
                    format!("{}: ", self.line)
                } else {
                    String::new()
                };
                self.original_bytes += (prefix.len() + character.len_utf8()) as u64;
                let added_chars = prefix.chars().count() + 1;
                if self.truncated
                    || self.visible_chars + added_chars > READ_FILE_MAX_CHARS
                    || (self.at_line_start && self.output_lines >= READ_FILE_MAX_LINES)
                {
                    self.truncated = true;
                } else {
                    self.output.push_str(&prefix);
                    self.output.push(character);
                    self.visible_chars += added_chars;
                    self.next_offset = self.offset + 1;
                    if character == '\n' {
                        self.output_lines += 1;
                    }
                }
            } else if !self.truncated
                && self.requested_offset.is_none()
                && self.line < self.start_line
            {
                self.next_offset = self.offset + 1;
            }
            if character == '\n' {
                self.lf += 1;
                if self.previous == '\r' {
                    self.crlf += 1;
                }
                self.line += 1;
            }
            self.at_line_start = character == '\n';
            self.previous = character;
            self.offset += 1;
        }
    }
}

fn normalize_cursor_path(path: &str) -> String {
    let mut normalized = path.trim().replace('\\', "/");
    while let Some(stripped) = normalized.strip_prefix("./") {
        normalized = stripped.to_string();
    }
    normalized
}
