mod error;
mod format;
mod local_rg;
mod request;

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::tools::base::{ToolContext, ToolSpec};
use crate::tools::common::{
    grep_text, is_hidden_path, is_ignored_root, matches_file_type, path_escapes_workspace_error,
    GrepTextOptions,
};
use crate::types::{ToolArguments, ToolDirective, ToolExecutionResult, ToolResultStatus};
use crate::workspace::LocalWorkspaceBackend;

use error::grep_error;
use format::{
    cap_file_counts, cap_file_paths, cap_match_rows, render_grep_content, truncate_result_text,
    MAX_STRUCTURED_CHARS, MAX_STRUCTURED_ITEMS,
};
use local_rg::{
    is_workspace_root_path, resolve_rg_executable, workspace_grep_local_rg, RgWorkspaceGrepRequest,
};
use request::{parse_workspace_grep_request, WorkspaceGrepRequest};

pub fn workspace_grep(context: &mut ToolContext, arguments: &ToolArguments) -> ToolExecutionResult {
    let spec = workspace_grep_tool();
    (spec.handler)(context, arguments)
}

pub(crate) fn workspace_grep_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "workspace_grep",
        "Search workspace files with grep-style semantics.",
        Arc::new(|context, arguments| {
            let request = match parse_workspace_grep_request(arguments) {
                Ok(request) => request,
                Err(error) => return grep_error(error),
            };
            let WorkspaceGrepRequest {
                pattern,
                output_mode,
                file_type,
                path,
                glob_pattern,
                include_hidden,
                include_ignored,
                multiline,
                show_line_numbers,
                before_context,
                after_context,
                head_limit,
                case_insensitive,
                regex,
            } = request;

            if let Err(error) = context.resolve_workspace_path(&path) {
                return path_escapes_workspace_error(error);
            }
            let backend = context.effective_workspace_backend();
            let explicit_file_target = backend.is_file(&path);
            let mut searched_files = 0usize;
            let mut total_matches = 0usize;
            let mut files_with_matches = Vec::<String>::new();
            let mut file_counts = BTreeMap::<String, usize>::new();
            let mut rows = Vec::<Value>::new();

            let rg_result = if explicit_file_target {
                None
            } else {
                backend
                    .as_any()
                    .downcast_ref::<LocalWorkspaceBackend>()
                    .and_then(|_| {
                        let rg_executable = resolve_rg_executable()?;
                        workspace_grep_local_rg(RgWorkspaceGrepRequest {
                            context,
                            path: &path,
                            glob_pattern: &glob_pattern,
                            pattern: &pattern,
                            output_mode: &output_mode,
                            file_type: file_type.as_deref(),
                            case_insensitive,
                            multiline,
                            before_context,
                            after_context,
                            include_hidden,
                            include_ignored,
                            rg_executable: &rg_executable,
                        })
                    })
            };

            if let Some(result) = rg_result {
                searched_files = result.files_searched;
                total_matches = result.total_matches;
                files_with_matches = result.files_with_matches;
                file_counts = result.file_counts;
                rows = result.rows;
            } else {
                let candidate_files = if explicit_file_target {
                    let display_path = backend
                        .file_info(&path)
                        .ok()
                        .flatten()
                        .map(|info| info.path)
                        .unwrap_or_else(|| path.replace('\\', "/"));
                    vec![(path.clone(), display_path)]
                } else {
                    match backend.list_files(&path, &glob_pattern) {
                        Ok(files) => files
                            .into_iter()
                            .map(|file_path| (file_path.clone(), file_path))
                            .collect(),
                        Err(error) => return grep_error(error.to_string()),
                    }
                };

                for (read_path, relative_path) in candidate_files {
                    if !explicit_file_target && !include_hidden && is_hidden_path(&relative_path) {
                        continue;
                    }
                    if !explicit_file_target
                        && !include_ignored
                        && is_workspace_root_path(&path)
                        && relative_path.split('/').next().is_some_and(is_ignored_root)
                    {
                        continue;
                    }
                    if !matches_file_type(&relative_path, file_type.as_deref()) {
                        continue;
                    }
                    let Ok(text) = backend.read_text(&read_path) else {
                        continue;
                    };
                    searched_files += 1;
                    let grep_options = GrepTextOptions {
                        multiline,
                        before_context,
                        after_context,
                        show_line_numbers,
                    };
                    let grep_result = grep_text(&relative_path, &text, &regex, grep_options);
                    let match_count = grep_result.match_count;
                    if match_count == 0 {
                        continue;
                    }
                    total_matches += match_count;
                    files_with_matches.push(relative_path.clone());
                    file_counts.insert(relative_path, match_count);
                    rows.extend(grep_result.rows);
                }
            }

            files_with_matches.sort();
            let files_with_match_count = files_with_matches.len();
            let total_result_items = match output_mode.as_str() {
                "files_with_matches" => files_with_matches.len(),
                "count" => file_counts.len(),
                _ => rows.len(),
            };
            let mut head_limited = false;
            let structured_capped;
            if let Some(limit) = head_limit {
                match output_mode.as_str() {
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
            match output_mode.as_str() {
                "files_with_matches" => {
                    let (capped_files, capped) = cap_file_paths(files_with_matches);
                    files_with_matches = capped_files;
                    structured_capped = capped;
                }
                "count" => {
                    let (capped_counts, capped) = cap_file_counts(file_counts);
                    file_counts = capped_counts;
                    structured_capped = capped;
                }
                _ => {
                    let (capped_rows, capped) = cap_match_rows(rows);
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
                "output_mode": output_mode.clone(),
                "head_limit": head_limit,
                "head_limited": head_limited,
                "total_result_items": total_result_items,
                "returned_count": match output_mode.as_str() {
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
            match output_mode.as_str() {
                "files_with_matches" => payload["files"] = json!(files_with_matches),
                "count" => payload["file_counts"] = json!(file_counts),
                _ => payload["matches"] = Value::Array(rows),
            }
            let content = render_grep_content(
                &output_mode,
                &pattern,
                &payload,
                show_line_numbers,
                structured_truncated,
            );
            let (content, content_truncated) =
                truncate_result_text(content, total_matches, files_with_match_count);
            payload["content_truncated"] = json!(content_truncated);
            payload["truncated"] = json!(content_truncated || structured_truncated);
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
    if let Some(schema) = crate::tools::schemas::schema_for("workspace_grep") {
        spec.schema = schema;
    }
    spec
}
