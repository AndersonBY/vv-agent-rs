use std::sync::Arc;

use serde_json::json;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::tools::base::{ToolContext, ToolSpec};
use crate::tools::common::{
    coerce_truthy_arg, path_escapes_workspace_error, replace_n, stringify_tool_arg,
};
use crate::types::{Metadata, ToolArguments, ToolExecutionResult, ToolResultStatus};

use super::workspace_backend_error;

const FILE_BASELINES_STATE_KEY: &str = "_workspace_file_baselines";
const UTF8_BOM: &[u8] = b"\xef\xbb\xbf";
pub(crate) const READ_FILE_BASELINE_SOURCE: &str = "read_file";
pub(crate) const WRITE_FILE_BASELINE_SOURCE: &str = "write_file";
pub(crate) const EDIT_FILE_BASELINE_SOURCE: &str = "edit_file";
pub(crate) const EDIT_FILE_ALLOWED_BASELINE_SOURCES: &[&str] = &[
    READ_FILE_BASELINE_SOURCE,
    WRITE_FILE_BASELINE_SOURCE,
    EDIT_FILE_BASELINE_SOURCE,
];
pub(crate) const WRITE_FILE_ALLOWED_BASELINE_SOURCES: &[&str] = &[
    READ_FILE_BASELINE_SOURCE,
    WRITE_FILE_BASELINE_SOURCE,
    EDIT_FILE_BASELINE_SOURCE,
];
const MAX_DIFF_CHARS: usize = 12_000;

pub fn edit_file(context: &mut ToolContext, arguments: &ToolArguments) -> ToolExecutionResult {
    let spec = edit_file_tool();
    (spec.handler)(context, arguments)
}

pub(crate) fn edit_file_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "edit_file",
        "Safely edit an existing workspace file.",
        Arc::new(|context, arguments| {
            let missing_arguments = ["path", "old_string", "new_string"]
                .into_iter()
                .filter(|name| !arguments.contains_key(*name))
                .collect::<Vec<_>>();
            if !missing_arguments.is_empty() {
                return workspace_tool_error_with_details(
                    "`path`, `old_string`, and `new_string` are required.",
                    "invalid_arguments",
                    Metadata::from([("missing_arguments".to_string(), json!(missing_arguments))]),
                );
            }
            let path = stringify_tool_arg(arguments.get("path"), "");
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
            let old_string = stringify_tool_arg(arguments.get("old_string"), "");
            if old_string.is_empty() {
                return workspace_tool_error(
                    "`old_string` cannot be empty.",
                    "old_string_empty",
                    &path,
                );
            }
            let new_string = stringify_tool_arg(arguments.get("new_string"), "");
            if old_string == new_string {
                return workspace_tool_error(
                    "No changes: old_string and new_string are identical.",
                    "no_changes",
                    &path,
                );
            }
            let replace_all = coerce_truthy_arg(arguments.get("replace_all"), false);

            match backend.read_bytes(&path) {
                Ok(raw) => {
                    let (text, has_bom) = match decode_workspace_text(&raw) {
                        Ok(decoded) => decoded,
                        Err(_) => {
                            return workspace_tool_error(
                                "Unsupported file encoding for edit_file.",
                                "unsupported_encoding",
                                &path,
                            )
                        }
                    };
                    if let Some(issue) = baseline_issue(
                        context,
                        &path,
                        &raw,
                        EDIT_FILE_ALLOWED_BASELINE_SOURCES,
                        true,
                    ) {
                        let message = if issue == "file_changed_since_read" {
                            "File changed since it was last read. Re-read it before editing."
                        } else {
                            "Read the file with read_file before editing."
                        };
                        return workspace_tool_error(message, issue, &path);
                    }

                    let line_ending = detect_line_ending(&text);
                    let mut actual_old = old_string.clone();
                    let mut actual_new = new_string.clone();
                    let mut occurrence_count = text.matches(&actual_old).count();
                    if occurrence_count == 0
                        && line_ending == "crlf"
                        && !old_string.contains("\r\n")
                    {
                        actual_old = old_string.replace('\n', "\r\n");
                        actual_new = new_string.replace('\n', "\r\n");
                        occurrence_count = text.matches(&actual_old).count();
                    }
                    if occurrence_count == 0 {
                        return workspace_tool_error(
                            "`old_string` not found in file.",
                            "old_string_not_found",
                            &path,
                        );
                    }
                    if occurrence_count > 1 && !replace_all {
                        return workspace_tool_error_with_details(
                            "`old_string` matched multiple locations; make it unique or set replace_all=true.",
                            "old_string_not_unique",
                            Metadata::from([
                                ("path".to_string(), json!(path)),
                                ("match_count".to_string(), json!(occurrence_count)),
                            ]),
                        );
                    }
                    let replaced_count = if replace_all { occurrence_count } else { 1 };
                    let replaced_text = if replace_all {
                        text.replace(&actual_old, &actual_new)
                    } else {
                        replace_n(&text, &actual_old, &actual_new, 1)
                    };
                    let encoded_text = if has_bom {
                        format!("\u{feff}{replaced_text}")
                    } else {
                        replaced_text.clone()
                    };
                    match backend.write_text(&path, &encoded_text, false) {
                        Ok(_) => {
                            record_file_baseline(
                                context,
                                &path,
                                encoded_text.as_bytes(),
                                false,
                                EDIT_FILE_BASELINE_SOURCE,
                            );
                            edit_success_result(
                                &path,
                                &text,
                                &replaced_text,
                                replaced_count,
                                line_ending,
                            )
                        }
                        Err(error) => workspace_backend_error(error),
                    }
                }
                Err(error) => workspace_backend_error(error),
            }
        }),
    );
    if let Some(schema) = crate::tools::schemas::schema_for("edit_file") {
        spec.schema = schema;
    }
    spec
}

pub(crate) fn workspace_tool_error(
    message: impl Into<String>,
    error_code: impl Into<String>,
    path: &str,
) -> ToolExecutionResult {
    workspace_tool_error_with_details(
        message,
        error_code,
        Metadata::from([("path".to_string(), json!(path))]),
    )
}

pub(crate) fn workspace_tool_error_with_details(
    message: impl Into<String>,
    error_code: impl Into<String>,
    details: Metadata,
) -> ToolExecutionResult {
    let message = message.into();
    let error_code = error_code.into();
    let mut payload = serde_json::Map::new();
    payload.insert("ok".to_string(), Value::Bool(false));
    payload.insert("error".to_string(), Value::String(message.clone()));
    payload.insert("error_code".to_string(), Value::String(error_code.clone()));
    payload.insert("message".to_string(), Value::String(message));
    payload.extend(details.clone());
    let mut metadata = details;
    metadata.insert("error_code".to_string(), Value::String(error_code.clone()));
    ToolExecutionResult {
        tool_call_id: String::new(),
        content: Value::Object(payload).to_string(),
        status: ToolResultStatus::Error,
        directive: crate::types::ToolDirective::Continue,
        error_code: Some(error_code),
        metadata,
        image_url: None,
        image_path: None,
    }
}

pub(crate) fn decode_workspace_text(raw: &[u8]) -> Result<(String, bool), ()> {
    let has_bom = raw.starts_with(UTF8_BOM);
    let payload = if has_bom { &raw[UTF8_BOM.len()..] } else { raw };
    String::from_utf8(payload.to_vec())
        .map(|text| (text, has_bom))
        .map_err(|_| ())
}

pub(crate) fn record_file_baseline(
    context: &mut ToolContext,
    path: &str,
    raw: &[u8],
    is_partial: bool,
    source: &str,
) {
    let line_ending = String::from_utf8(raw.to_vec())
        .map(|text| detect_line_ending(&text).to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let entry = json!({
        "hash": content_hash(raw),
        "size": raw.len(),
        "line_ending": line_ending,
        "is_partial": is_partial,
        "source": source,
    });
    let baselines = context
        .shared_state
        .entry(FILE_BASELINES_STATE_KEY.to_string())
        .or_insert_with(|| Value::Object(Default::default()));
    if !baselines.is_object() {
        *baselines = Value::Object(Default::default());
    }
    baselines
        .as_object_mut()
        .expect("baselines object")
        .insert(path.to_string(), entry);
}

pub(crate) fn baseline_issue(
    context: &ToolContext,
    path: &str,
    current_raw: &[u8],
    allowed_sources: &[&str],
    allow_partial: bool,
) -> Option<&'static str> {
    let Some(baseline) = context
        .shared_state
        .get(FILE_BASELINES_STATE_KEY)
        .and_then(Value::as_object)
        .and_then(|object| object.get(path))
        .and_then(Value::as_object)
    else {
        return Some("file_not_read");
    };
    let Some(is_partial) = baseline.get("is_partial").and_then(Value::as_bool) else {
        return Some("file_not_read");
    };
    let Some(source) = baseline.get("source").and_then(Value::as_str) else {
        return Some("file_not_read");
    };
    if !allowed_sources.contains(&source) {
        return Some("file_not_read");
    }
    if is_partial && !(allow_partial && source == READ_FILE_BASELINE_SOURCE) {
        return Some("file_not_read");
    }
    let baseline_hash = baseline.get("hash").and_then(Value::as_str);
    if baseline_hash != Some(content_hash(current_raw).as_str()) {
        return Some("file_changed_since_read");
    }
    None
}

pub(crate) fn changed_file_metadata(
    path: &str,
    before: &str,
    after: &str,
    operation: &str,
    line_ending: &str,
) -> Metadata {
    let (diff, diff_truncated, additions, deletions) = bounded_diff(path, before, after);
    let mut metadata = Metadata::new();
    metadata.insert("changed_files".to_string(), json!([path]));
    metadata.insert("diff".to_string(), Value::String(diff));
    metadata.insert("additions".to_string(), json!(additions));
    metadata.insert("deletions".to_string(), json!(deletions));
    metadata.insert(
        "operation".to_string(),
        Value::String(operation.to_string()),
    );
    metadata.insert(
        "line_ending".to_string(),
        Value::String(line_ending.to_string()),
    );
    metadata.insert("diff_truncated".to_string(), Value::Bool(diff_truncated));
    metadata
}

pub(crate) fn detect_line_ending(text: &str) -> &'static str {
    let lf_count = text.matches('\n').count();
    let crlf_count = text.matches("\r\n").count();
    if crlf_count > 0 && crlf_count == lf_count {
        "crlf"
    } else if crlf_count > 0 {
        "mixed"
    } else {
        "lf"
    }
}

fn edit_success_result(
    path: &str,
    before: &str,
    after: &str,
    replaced_count: usize,
    line_ending: &str,
) -> ToolExecutionResult {
    ToolExecutionResult {
        tool_call_id: String::new(),
        content: json!({
            "ok": true,
            "path": path,
            "replaced_count": replaced_count,
        })
        .to_string(),
        status: ToolResultStatus::Success,
        directive: crate::types::ToolDirective::Continue,
        error_code: None,
        metadata: changed_file_metadata(path, before, after, "edit_file", line_ending),
        image_url: None,
        image_path: None,
    }
}

fn content_hash(raw: &[u8]) -> String {
    let digest = Sha256::digest(raw);
    format!("{digest:x}")
}

fn bounded_diff(path: &str, before: &str, after: &str) -> (String, bool, usize, usize) {
    let before_lines = split_unified_diff_lines(before);
    let after_lines = split_unified_diff_lines(after);
    let mut prefix = 0;
    while prefix < before_lines.len()
        && prefix < after_lines.len()
        && before_lines[prefix] == after_lines[prefix]
    {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix + prefix < before_lines.len()
        && suffix + prefix < after_lines.len()
        && before_lines[before_lines.len() - 1 - suffix]
            == after_lines[after_lines.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let before_changed_end = before_lines.len().saturating_sub(suffix);
    let after_changed_end = after_lines.len().saturating_sub(suffix);
    let additions = after_changed_end.saturating_sub(prefix);
    let deletions = before_changed_end.saturating_sub(prefix);
    if additions == 0 && deletions == 0 {
        return (String::new(), false, 0, 0);
    }

    let context_start = prefix.saturating_sub(3);
    let before_hunk_end = before_changed_end.saturating_add(3).min(before_lines.len());
    let after_hunk_end = after_changed_end.saturating_add(3).min(after_lines.len());
    let before_hunk_count = before_hunk_end.saturating_sub(context_start);
    let after_hunk_count = after_hunk_end.saturating_sub(context_start);
    let mut diff = format!(
        "--- {path}\n+++ {path}\n@@ -{} +{} @@\n",
        format_unified_range(context_start, before_hunk_count),
        format_unified_range(context_start, after_hunk_count),
    );
    for line in &before_lines[context_start..prefix] {
        render_unified_line(&mut diff, ' ', line);
    }
    for line in &before_lines[prefix..before_changed_end] {
        render_unified_line(&mut diff, '-', line);
    }
    for line in &after_lines[prefix..after_changed_end] {
        render_unified_line(&mut diff, '+', line);
    }
    for line in &after_lines[after_changed_end..after_hunk_end] {
        render_unified_line(&mut diff, ' ', line);
    }

    let truncated = diff.chars().count() > MAX_DIFF_CHARS;
    if truncated {
        diff = diff.chars().take(MAX_DIFF_CHARS).collect();
    }
    (diff, truncated, additions, deletions)
}

fn split_unified_diff_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (index, character) in text.char_indices() {
        if character == '\n' {
            let end = index + character.len_utf8();
            lines.push(&text[start..end]);
            start = end;
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

fn format_unified_range(start_index: usize, count: usize) -> String {
    let start_line = if count == 0 {
        start_index
    } else {
        start_index + 1
    };
    if count == 1 {
        start_line.to_string()
    } else {
        format!("{start_line},{count}")
    }
}

fn render_unified_line(output: &mut String, marker: char, line: &str) {
    let has_newline = line.ends_with('\n');
    let mut body = if has_newline {
        &line[..line.len() - 1]
    } else {
        line
    };
    if has_newline && body.ends_with('\r') {
        body = &body[..body.len() - 1];
    }
    output.push(marker);
    output.push_str(body);
    output.push('\n');
    if !has_newline {
        output.push_str("\\ No newline at end of file\n");
    }
}
