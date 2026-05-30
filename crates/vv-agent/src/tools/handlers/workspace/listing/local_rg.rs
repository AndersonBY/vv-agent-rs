mod command;
mod paths;
mod scan;
#[cfg(all(test, unix))]
mod tests;
mod types;

use std::path::Path;

use crate::tools::base::ToolContext;

use super::request::ListFilesRequest;
use super::types::ListFilesOutcome;
use command::resolve_rg_executable;
use paths::local_ignored_root_names;
use scan::list_files_local_rg;
use types::RgListFilesRequest;

pub(super) fn list_files_with_rg(
    context: &ToolContext,
    resolved_path: &Path,
    root_listing: bool,
    request: &ListFilesRequest,
) -> Option<ListFilesOutcome> {
    if !resolved_path.is_dir() {
        return None;
    }
    let ignored_root_names = if root_listing && !request.include_ignored {
        local_ignored_root_names(resolved_path)
    } else {
        Vec::new()
    };
    let rg_executable = resolve_rg_executable()?;
    let result = list_files_local_rg(RgListFilesRequest {
        context,
        base_path: resolved_path,
        base_is_workspace_root: root_listing,
        glob: &request.glob,
        include_hidden: request.include_hidden,
        include_ignored: request.include_ignored,
        ignored_root_names: &ignored_root_names,
        max_results: request.max_results,
        scan_limit: request.scan_limit,
        rg_executable: &rg_executable,
    })?;
    Some(ListFilesOutcome::new(
        result.files,
        result.total_count,
        result.truncated,
        result.scan_limited,
        ignored_root_names,
    ))
}
