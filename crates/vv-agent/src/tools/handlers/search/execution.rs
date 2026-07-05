use crate::tools::base::ToolContext;
use crate::tools::common::{
    is_hidden_path, is_ignored_root, is_sensitive_path, matches_file_type,
    path_escapes_workspace_error, sensitive_path_is_covered_by_rg_excludes,
};
use crate::types::ToolExecutionResult;
use crate::workspace::{LocalWorkspaceBackend, WorkspaceBackend};

use super::error::grep_error;
use super::fallback::search_files_fallback;
use super::local_rg::{
    is_workspace_root_path, resolve_rg_executable, search_files_local_rg, RgGrepResult,
    RgSearchFilesRequest,
};
use super::request::SearchFilesRequest;
use super::response::search_files_success_response;

pub(super) fn execute_search_files(
    context: &mut ToolContext,
    request: SearchFilesRequest,
) -> ToolExecutionResult {
    if let Err(error) = context.resolve_workspace_path(&request.path) {
        return path_escapes_workspace_error(error);
    }

    let backend = context.effective_workspace_backend();
    let explicit_file_target = backend.is_file(&request.path);
    let sensitive_candidates = if request.include_sensitive {
        Vec::new()
    } else {
        match sensitive_candidate_paths(backend.as_ref(), &request, explicit_file_target) {
            Ok(paths) => paths,
            Err(error) => return grep_error(error.to_string()),
        }
    };
    let sensitive_files_omitted = sensitive_candidates.len();
    let can_exclude_sensitive_in_rg = request.include_sensitive
        || sensitive_candidates
            .iter()
            .all(|path| sensitive_path_is_safe_for_rg(&request, path));

    let rg_result = if explicit_file_target || !can_exclude_sensitive_in_rg {
        None
    } else {
        backend
            .as_any()
            .downcast_ref::<LocalWorkspaceBackend>()
            .and_then(|_| {
                let rg_executable = resolve_rg_executable()?;
                search_files_local_rg(RgSearchFilesRequest {
                    context,
                    path: &request.path,
                    glob_pattern: &request.glob_pattern,
                    pattern: &request.pattern,
                    output_mode: &request.output_mode,
                    file_type: request.file_type.as_deref(),
                    case_insensitive: request.case_insensitive,
                    literal: request.literal,
                    multiline: request.multiline,
                    before_context: request.before_context,
                    after_context: request.after_context,
                    include_hidden: request.include_hidden,
                    include_ignored: request.include_ignored,
                    include_sensitive: request.include_sensitive,
                    rg_executable: &rg_executable,
                })
            })
    };

    let result = match rg_result {
        Some(mut result) => {
            if !request.include_sensitive {
                filter_sensitive_rg_result(&mut result);
            }
            result.sensitive_files_omitted = sensitive_files_omitted;
            result
        }
        None => match search_files_fallback(backend.as_ref(), &request, explicit_file_target) {
            Ok(result) => result,
            Err(error) => return grep_error(error),
        },
    };
    search_files_success_response(&request, result)
}

fn sensitive_candidate_paths(
    backend: &dyn WorkspaceBackend,
    request: &SearchFilesRequest,
    explicit_file_target: bool,
) -> std::io::Result<Vec<String>> {
    let raw_paths = if explicit_file_target {
        let display_path = backend
            .file_info(&request.path)?
            .map(|info| info.path)
            .unwrap_or_else(|| request.path.replace('\\', "/"));
        vec![display_path]
    } else {
        backend.list_files(&request.path, &request.glob_pattern)?
    };

    Ok(raw_paths
        .into_iter()
        .map(|path| path.replace('\\', "/"))
        .filter(|path| matches_file_type(path, request.file_type.as_deref()))
        .filter(|path| is_sensitive_path(path))
        .collect())
}

fn sensitive_path_is_safe_for_rg(request: &SearchFilesRequest, path: &str) -> bool {
    sensitive_path_is_covered_by_rg_excludes(path)
        || (!request.include_hidden && is_hidden_path(path))
        || (!request.include_ignored
            && is_workspace_root_path(&request.path)
            && path.split('/').next().is_some_and(is_ignored_root))
}

fn filter_sensitive_rg_result(result: &mut RgGrepResult) {
    result
        .files_with_matches
        .retain(|path| !is_sensitive_path(path));
    result
        .file_counts
        .retain(|path, _count| !is_sensitive_path(path));
    result.rows.retain(|row| {
        row.get("path")
            .and_then(|path| path.as_str())
            .is_none_or(|path| !is_sensitive_path(path))
    });
    result.total_matches = result.file_counts.values().sum();
}

pub(super) fn empty_grep_result() -> RgGrepResult {
    RgGrepResult {
        files_searched: 0,
        total_matches: 0,
        files_with_matches: Vec::new(),
        file_counts: Default::default(),
        rows: Vec::new(),
        sensitive_files_omitted: 0,
    }
}
