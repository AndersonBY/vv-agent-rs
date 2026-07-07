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
const MAX_DIFF_CHARS: usize = 8_000;

pub fn edit_file(context: &mut ToolContext, arguments: &ToolArguments) -> ToolExecutionResult {
    let spec = edit_file_tool();
    (spec.handler)(context, arguments)
}

pub(crate) fn edit_file_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "edit_file",
        "Safely edit an existing workspace file.",
        Arc::new(|context, arguments| {
            if !arguments.contains_key("path")
                || !arguments.contains_key("old_string")
                || !arguments.contains_key("new_string")
            {
                let path = stringify_tool_arg(arguments.get("path"), "");
                return workspace_tool_error(
                    "`path`, `old_string`, and `new_string` are required.",
                    "invalid_arguments",
                    &path,
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
                    let text = match String::from_utf8(raw.clone()) {
                        Ok(text) => text,
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
                        return workspace_tool_error(
                            format!(
                                "`old_string` matched {occurrence_count} locations; make it unique or set replace_all=true."
                            ),
                            "old_string_not_unique",
                            &path,
                        );
                    }
                    let replaced_count = if replace_all { occurrence_count } else { 1 };
                    let replaced_text = if replace_all {
                        text.replace(&actual_old, &actual_new)
                    } else {
                        replace_n(&text, &actual_old, &actual_new, 1)
                    };
                    match backend.write_text(&path, &replaced_text, false) {
                        Ok(_) => {
                            record_file_baseline(
                                context,
                                &path,
                                replaced_text.as_bytes(),
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
    let message = message.into();
    let error_code = error_code.into();
    ToolExecutionResult {
        tool_call_id: String::new(),
        content: json!({
            "ok": false,
            "error": message,
            "message": message,
            "error_code": error_code,
            "path": path,
        })
        .to_string(),
        status: ToolResultStatus::Error,
        directive: crate::types::ToolDirective::Continue,
        error_code: Some(error_code),
        metadata: Metadata::new(),
        image_url: None,
        image_path: None,
    }
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
    if diff_truncated {
        metadata.insert("diff_truncated".to_string(), Value::Bool(true));
    }
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
    let before_lines = before.lines().collect::<Vec<_>>();
    let after_lines = after.lines().collect::<Vec<_>>();
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
    let before_changed = &before_lines[prefix..before_lines.len().saturating_sub(suffix)];
    let after_changed = &after_lines[prefix..after_lines.len().saturating_sub(suffix)];
    let mut diff = format!("--- {path}\n+++ {path}\n@@\n");
    for line in before_changed {
        diff.push('-');
        diff.push_str(line);
        diff.push('\n');
    }
    for line in after_changed {
        diff.push('+');
        diff.push_str(line);
        diff.push('\n');
    }
    let truncated = diff.len() > MAX_DIFF_CHARS;
    if truncated {
        diff.truncate(MAX_DIFF_CHARS);
    }
    (diff, truncated, after_changed.len(), before_changed.len())
}
