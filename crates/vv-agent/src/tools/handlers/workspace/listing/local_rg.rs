mod command;
mod paths;
mod scan;
#[cfg(all(test, unix))]
mod tests;
mod types;

use std::path::Path;

use crate::tools::base::ToolContext;

use super::request::FindFilesRequest;
use super::types::FindFilesOutcome;
use command::resolve_rg_executable;
use paths::local_ignored_root_names;
use scan::find_files_local_rg;
use types::RgFindFilesRequest;

pub(super) fn find_files_with_rg(
    context: &ToolContext,
    resolved_path: &Path,
    root_listing: bool,
    request: &FindFilesRequest,
) -> Option<FindFilesOutcome> {
    if !resolved_path.is_dir() {
        return None;
    }
    let ignored_root_names = if root_listing && !request.include_ignored {
        local_ignored_root_names(resolved_path)
    } else {
        Vec::new()
    };
    let rg_executable = resolve_rg_executable()?;
    let result = find_files_local_rg(RgFindFilesRequest {
        context,
        base_path: resolved_path,
        base_is_workspace_root: root_listing,
        glob: &request.glob,
        include_hidden: request.include_hidden,
        include_ignored: request.include_ignored,
        include_sensitive: request.include_sensitive,
        ignored_root_names: &ignored_root_names,
        scan_limit: request.scan_limit,
        rg_executable: &rg_executable,
    })?;
    let (files, effective_sort) = sort_files(context, result.files, &request.sort);
    let count = result.total_count;
    let returned = files
        .into_iter()
        .skip(request.offset)
        .take(request.max_results)
        .collect::<Vec<_>>();
    let truncated = result.scan_limited || request.offset.saturating_add(returned.len()) < count;
    Some(FindFilesOutcome::new(
        returned,
        count,
        truncated || result.truncated,
        result.scan_limited,
        ignored_root_names,
        effective_sort,
        result.sensitive_files_omitted,
    ))
}

fn sort_files(
    context: &ToolContext,
    mut files: Vec<String>,
    requested_sort: &str,
) -> (Vec<String>, String) {
    if requested_sort == "path_asc" {
        files.sort();
        return (files, "path_asc".to_string());
    }

    let mut stat_rows = Vec::with_capacity(files.len());
    for file in &files {
        let Ok(path) = context.resolve_workspace_path(file) else {
            files.sort();
            return (files, "path_asc".to_string());
        };
        let Ok(metadata) = path.metadata() else {
            files.sort();
            return (files, "path_asc".to_string());
        };
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        stat_rows.push((modified, file.clone()));
    }
    stat_rows.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    (
        stat_rows.into_iter().map(|(_, path)| path).collect(),
        "modified_desc".to_string(),
    )
}
