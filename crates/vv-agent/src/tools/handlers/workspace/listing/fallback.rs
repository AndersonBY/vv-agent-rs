use std::io;
use std::sync::Arc;

use crate::tools::common::{
    collect_ignored_roots, is_hidden_path, is_ignored_root, is_sensitive_path,
};
use crate::workspace::WorkspaceBackend;

use super::request::FindFilesRequest;
use super::types::FindFilesOutcome;

pub(super) fn find_files_with_backend(
    backend: &Arc<dyn WorkspaceBackend>,
    request: &FindFilesRequest,
    is_local_backend: bool,
    root_listing: bool,
) -> io::Result<FindFilesOutcome> {
    let mut files = backend.list_files(&request.path, &request.glob)?;
    let summarize_ignored_roots = is_local_backend && root_listing && !request.include_ignored;
    let ignored_roots = if summarize_ignored_roots {
        collect_ignored_roots(&files)
    } else {
        Vec::new()
    };
    if summarize_ignored_roots {
        files.retain(|path| {
            path.split('/')
                .next()
                .is_none_or(|root| !is_ignored_root(root))
        });
    }
    if !request.include_hidden {
        files.retain(|path| !is_hidden_path(path));
    }
    let mut sensitive_files_omitted = 0usize;
    if !request.include_sensitive {
        files.retain(|path| {
            if is_sensitive_path(path) {
                sensitive_files_omitted += 1;
                false
            } else {
                true
            }
        });
    }

    files.sort();
    let effective_sort = "path_asc".to_string();
    let scan_limited = files.len() > request.scan_limit;
    if scan_limited {
        files.truncate(request.scan_limit);
    }
    let count = files.len();
    let returned = files
        .into_iter()
        .skip(request.offset)
        .take(request.max_results)
        .collect::<Vec<_>>();
    let truncated = scan_limited || request.offset.saturating_add(returned.len()) < count;

    Ok(FindFilesOutcome::new(
        returned,
        count,
        truncated,
        scan_limited,
        ignored_roots,
        effective_sort,
        sensitive_files_omitted,
    ))
}
