use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::tools::base::ToolSpec;
use crate::tools::common::{
    collect_workspace_files, grep_text, is_hidden_path, is_ignored_root, is_supported_file_type,
    matches_file_type, resolve_workspace_path, tool_error, tool_result,
    workspace_relative_path_or_absolute, GrepTextOptions,
};
use crate::types::{ToolDirective, ToolResultStatus};

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

            let target_path = resolve_workspace_path(&context.workspace, path);
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
            let total_result_items = match output_mode {
                "files_with_matches" => files_with_matches.len(),
                "count" => file_counts.len(),
                _ => rows.len(),
            };
            let mut head_limited = false;
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

            let summary = json!({
                "files_searched": searched_files,
                "files_with_matches": file_counts.len(),
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
                "truncated": head_limited,
            });
            match output_mode {
                "files_with_matches" => payload["files"] = json!(files_with_matches),
                "count" => payload["file_counts"] = json!(file_counts),
                _ => payload["matches"] = Value::Array(rows),
            }
            tool_result(
                ToolResultStatus::Success,
                payload,
                None,
                ToolDirective::Continue,
            )
        }),
    );
    if let Some(schema) = super::super::schemas::schema_for("workspace_grep") {
        spec.schema = schema;
    }
    spec
}
