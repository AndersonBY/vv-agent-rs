use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::tools::base::ToolSpec;
use crate::tools::common::{
    collect_workspace_files, grep_text, is_hidden_path, is_ignored_root, is_supported_file_type,
    matches_file_type, path_escapes_workspace_error, tool_error,
    workspace_relative_path_or_absolute, GrepTextOptions,
};
use crate::types::{ToolDirective, ToolExecutionResult, ToolResultStatus};

const MAX_STRUCTURED_ITEMS: usize = 200;
const MAX_STRUCTURED_CHARS: usize = 20_000;

pub(crate) fn workspace_grep_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "workspace_grep",
        "Search workspace files with grep-style semantics.",
        Arc::new(|context, arguments| {
            let pattern = arguments
                .get("pattern")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            if pattern.is_empty() {
                return tool_error("Search pattern is required");
            }
            let output_mode = arguments
                .get("output_mode")
                .and_then(Value::as_str)
                .unwrap_or("content");
            if !matches!(output_mode, "content" | "files_with_matches" | "count") {
                return tool_error(format!(
                    "Invalid `output_mode`: {output_mode}. Supported: content, count, files_with_matches"
                ));
            }
            let file_type = arguments
                .get("type")
                .and_then(Value::as_str)
                .map(|value| value.trim().to_ascii_lowercase())
                .filter(|value| !value.is_empty());
            if let Some(file_type) = &file_type {
                if !is_supported_file_type(file_type) {
                    return tool_error(format!("Unsupported file type: {file_type}"));
                }
            }
            let path = arguments.get("path").and_then(Value::as_str).unwrap_or(".");
            let include_hidden = arguments
                .get("include_hidden")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let include_ignored = arguments
                .get("include_ignored")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let multiline = arguments
                .get("multiline")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let show_line_numbers = arguments.get("n").and_then(Value::as_bool).unwrap_or(true);
            let context_lines = arguments
                .get("c")
                .and_then(Value::as_u64)
                .map(|value| value as usize);
            let before_context = context_lines
                .or_else(|| {
                    arguments
                        .get("b")
                        .and_then(Value::as_u64)
                        .map(|v| v as usize)
                })
                .unwrap_or(0);
            let after_context = context_lines
                .or_else(|| {
                    arguments
                        .get("a")
                        .and_then(Value::as_u64)
                        .map(|v| v as usize)
                })
                .unwrap_or(0);
            let head_limit = arguments
                .get("head_limit")
                .or_else(|| arguments.get("max_results"))
                .and_then(Value::as_u64)
                .map(|value| value.max(1) as usize);
            let case_insensitive = if let Some(case_sensitive) =
                arguments.get("case_sensitive").and_then(Value::as_bool)
            {
                !case_sensitive
            } else if let Some(force_insensitive) = arguments.get("i").and_then(Value::as_bool) {
                force_insensitive
            } else {
                !pattern.chars().any(char::is_uppercase)
            };

            let target_path = match context.resolve_workspace_path(path) {
                Ok(path) => path,
                Err(error) => return path_escapes_workspace_error(error),
            };
            let mut candidate_files = Vec::new();
            if target_path.is_file() {
                candidate_files.push(target_path);
            } else {
                match collect_workspace_files(&target_path) {
                    Ok(files) => candidate_files = files,
                    Err(error) => return tool_error(error.to_string()),
                }
            }

            let mut searched_files = 0usize;
            let mut total_matches = 0usize;
            let mut files_with_matches = Vec::<String>::new();
            let mut file_counts = BTreeMap::<String, usize>::new();
            let mut rows = Vec::<Value>::new();

            for file_path in candidate_files {
                let relative_path =
                    workspace_relative_path_or_absolute(&context.workspace, &file_path);
                if !include_hidden && is_hidden_path(&relative_path) {
                    continue;
                }
                if !include_ignored
                    && path == "."
                    && relative_path.split('/').next().is_some_and(is_ignored_root)
                {
                    continue;
                }
                if !matches_file_type(&relative_path, file_type.as_deref()) {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&file_path) else {
                    continue;
                };
                searched_files += 1;
                let grep_options = GrepTextOptions {
                    case_insensitive,
                    multiline,
                    before_context,
                    after_context,
                    show_line_numbers,
                };
                let file_match_rows = grep_text(&relative_path, &text, &pattern, grep_options);
                let match_count = file_match_rows
                    .iter()
                    .filter(|row| {
                        row.get("is_match")
                            .and_then(Value::as_bool)
                            .unwrap_or(false)
                    })
                    .count();
                if match_count == 0 {
                    continue;
                }
                total_matches += match_count;
                files_with_matches.push(relative_path.clone());
                file_counts.insert(relative_path, match_count);
                rows.extend(file_match_rows);
            }

            files_with_matches.sort();
            let files_with_match_count = files_with_matches.len();
            let total_result_items = match output_mode {
                "files_with_matches" => files_with_matches.len(),
                "count" => file_counts.len(),
                _ => rows.len(),
            };
            let mut head_limited = false;
            let structured_capped;
            if let Some(limit) = head_limit {
                match output_mode {
                    "files_with_matches" => {
                        head_limited = files_with_matches.len() > limit;
                        files_with_matches.truncate(limit);
                    }
                    "count" => {
                        head_limited = file_counts.len() > limit;
                        if head_limited {
                            file_counts = file_counts.into_iter().take(limit).collect();
                        }
                    }
                    _ => {
                        head_limited = rows.len() > limit;
                        rows.truncate(limit);
                    }
                }
            }
            match output_mode {
                "files_with_matches" => {
                    let (capped_files, capped) = cap_structured_items(files_with_matches, |path| {
                        estimate_file_path_size(path)
                    });
                    files_with_matches = capped_files;
                    structured_capped = capped;
                }
                "count" => {
                    let count_items = file_counts.into_iter().collect::<Vec<_>>();
                    let (capped_items, capped) =
                        cap_structured_items(count_items, estimate_file_count_size);
                    file_counts = capped_items.into_iter().collect();
                    structured_capped = capped;
                }
                _ => {
                    let (capped_rows, capped) = cap_structured_items(rows, estimate_match_row_size);
                    rows = capped_rows;
                    structured_capped = capped;
                }
            }
            let structured_truncated = head_limited || structured_capped;

            let summary = json!({
                "files_searched": searched_files,
                "files_with_matches": files_with_match_count,
                "total_matches": total_matches,
            });
            let mut payload = json!({
                "summary": summary,
                "pattern": pattern,
                "output_mode": output_mode,
                "head_limit": head_limit,
                "head_limited": head_limited,
                "total_result_items": total_result_items,
                "returned_count": match output_mode {
                    "files_with_matches" => files_with_matches.len(),
                    "count" => file_counts.len(),
                    _ => rows.len(),
                },
                "content_truncated": false,
                "structured_truncated": structured_truncated,
                "truncated": structured_truncated,
            });
            if structured_capped {
                payload["structured_item_limit"] = json!(MAX_STRUCTURED_ITEMS);
                payload["structured_char_limit"] = json!(MAX_STRUCTURED_CHARS);
            }
            match output_mode {
                "files_with_matches" => payload["files"] = json!(files_with_matches),
                "count" => payload["file_counts"] = json!(file_counts),
                _ => payload["matches"] = Value::Array(rows),
            }
            let content = render_grep_content(
                output_mode,
                &pattern,
                &payload,
                show_line_numbers,
                structured_truncated,
            );
            let metadata = payload
                .as_object()
                .map(|object| {
                    object
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect()
                })
                .unwrap_or_default();
            ToolExecutionResult {
                tool_call_id: String::new(),
                content,
                status: ToolResultStatus::Success,
                directive: ToolDirective::Continue,
                error_code: None,
                metadata,
                image_url: None,
                image_path: None,
            }
        }),
    );
    if let Some(schema) = super::super::schemas::schema_for("workspace_grep") {
        spec.schema = schema;
    }
    spec
}

fn render_grep_content(
    output_mode: &str,
    pattern: &str,
    payload: &Value,
    show_line_numbers: bool,
    head_limited: bool,
) -> String {
    let summary = &payload["summary"];
    let total_matches = summary["total_matches"].as_u64().unwrap_or_default();
    let files_with_matches = summary["files_with_matches"].as_u64().unwrap_or_default();
    match output_mode {
        "files_with_matches" => {
            let files = payload["files"].as_array().cloned().unwrap_or_default();
            let mut lines = vec![format!(
                "Found {files_with_matches} files matching pattern {pattern:?}"
            )];
            if files.is_empty() {
                lines.push("No matches found.".to_string());
            } else {
                if head_limited {
                    lines.push(format!("Showing first {} files.", files.len()));
                }
                lines.extend(
                    files
                        .into_iter()
                        .filter_map(|file| file.as_str().map(str::to_string)),
                );
            }
            lines.join("\n")
        }
        "count" => {
            let mut lines = vec![format!("Match counts for pattern {pattern:?}")];
            if head_limited {
                lines.push(format!(
                    "Showing first {} files.",
                    payload["file_counts"]
                        .as_object()
                        .map_or(0, |items| items.len())
                ));
            }
            if let Some(counts) = payload["file_counts"].as_object() {
                for (file, count) in counts {
                    lines.push(format!("{}: {}", file, count.as_u64().unwrap_or_default()));
                }
            }
            lines.push(format!(
                "Total: {total_matches} matches in {files_with_matches} files"
            ));
            lines.join("\n")
        }
        _ => {
            let mut lines = vec![format!(
                "Found {total_matches} matches in {files_with_matches} files for pattern {pattern:?}"
            )];
            let rows = payload["matches"].as_array().cloned().unwrap_or_default();
            if rows.is_empty() {
                lines.push("No matches found.".to_string());
                return lines.join("\n");
            }
            if head_limited {
                lines.push(format!("Showing first {} rows.", rows.len()));
            }
            let mut current_file = String::new();
            for row in rows {
                let row_path = row["path"].as_str().unwrap_or_default();
                if current_file != row_path {
                    lines.push(format!("File: {row_path}"));
                    current_file = row_path.to_string();
                }
                let marker = if row["is_match"].as_bool().unwrap_or(false) {
                    ""
                } else {
                    "-"
                };
                let text = row["text"].as_str().unwrap_or_default();
                if show_line_numbers {
                    let line = row["line"].as_u64().unwrap_or_default();
                    lines.push(format!("  {marker}{line}: {text}"));
                } else {
                    lines.push(format!("  {marker}{text}"));
                }
            }
            lines.join("\n")
        }
    }
}

fn estimate_match_row_size(row: &Value) -> usize {
    row["path"].as_str().map_or(0, str::len)
        + row["line"]
            .as_u64()
            .map_or(0, |line| line.to_string().len())
        + row["text"].as_str().map_or(0, str::len)
        + 32
}

fn estimate_file_path_size(path: &str) -> usize {
    path.len() + 4
}

fn estimate_file_count_size((path, count): &(String, usize)) -> usize {
    path.len() + count.to_string().len() + 8
}

fn cap_structured_items<T>(items: Vec<T>, estimator: impl Fn(&T) -> usize) -> (Vec<T>, bool) {
    let mut capped = Vec::new();
    let mut used_chars = 0usize;

    for item in items {
        let item_size = estimator(&item).max(1);
        if !capped.is_empty()
            && (capped.len() >= MAX_STRUCTURED_ITEMS
                || used_chars.saturating_add(item_size) > MAX_STRUCTURED_CHARS)
        {
            return (capped, true);
        }
        capped.push(item);
        used_chars = used_chars.saturating_add(item_size);
    }

    (capped, false)
}
