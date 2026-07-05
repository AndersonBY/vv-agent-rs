use crate::tools::common::{
    grep_text, is_hidden_path, is_ignored_root, is_sensitive_path, matches_file_type,
    GrepTextOptions,
};
use crate::workspace::WorkspaceBackend;

use super::execution::empty_grep_result;
use super::local_rg::{is_workspace_root_path, RgGrepResult};
use super::request::SearchFilesRequest;

pub(super) fn search_files_fallback(
    backend: &dyn WorkspaceBackend,
    request: &SearchFilesRequest,
    explicit_file_target: bool,
) -> Result<RgGrepResult, String> {
    let candidate_files = if explicit_file_target {
        let display_path = backend
            .file_info(&request.path)
            .ok()
            .flatten()
            .map(|info| info.path)
            .unwrap_or_else(|| request.path.replace('\\', "/"));
        vec![(request.path.clone(), display_path)]
    } else {
        backend
            .list_files(&request.path, &request.glob_pattern)
            .map(|files| {
                files
                    .into_iter()
                    .map(|file_path| (file_path.clone(), file_path))
                    .collect()
            })
            .map_err(|error| error.to_string())?
    };

    let mut result = empty_grep_result();
    for (read_path, relative_path) in candidate_files {
        if !request.include_sensitive && is_sensitive_path(&relative_path) {
            result.sensitive_files_omitted += 1;
            continue;
        }
        if !explicit_file_target && !request.include_hidden && is_hidden_path(&relative_path) {
            continue;
        }
        if !explicit_file_target
            && !request.include_ignored
            && is_workspace_root_path(&request.path)
            && relative_path.split('/').next().is_some_and(is_ignored_root)
        {
            continue;
        }
        if !matches_file_type(&relative_path, request.file_type.as_deref()) {
            continue;
        }
        let Ok(text) = backend.read_text(&read_path) else {
            continue;
        };
        result.files_searched += 1;
        let grep_options = GrepTextOptions {
            multiline: request.multiline,
            before_context: request.before_context,
            after_context: request.after_context,
            show_line_numbers: request.show_line_numbers,
        };
        let grep_result = grep_text(&relative_path, &text, &request.regex, grep_options);
        let match_count = grep_result.match_count;
        if match_count == 0 {
            continue;
        }
        result.total_matches += match_count;
        result.files_with_matches.push(relative_path.clone());
        result.file_counts.insert(relative_path, match_count);
        result.rows.extend(grep_result.rows);
    }

    Ok(result)
}
