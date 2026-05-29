use std::io;
use std::sync::Arc;

use crate::tools::common::{collect_ignored_roots, is_hidden_path, is_ignored_root};
use crate::workspace::WorkspaceBackend;

use super::request::ListFilesRequest;
use super::types::ListFilesOutcome;

pub(super) fn list_files_with_backend(
    backend: &Arc<dyn WorkspaceBackend>,
    request: &ListFilesRequest,
    is_local_backend: bool,
    root_listing: bool,
) -> io::Result<ListFilesOutcome> {
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

    let actual_count = files.len();
    let scan_limited = actual_count > request.scan_limit;
    let count = if scan_limited {
        request.scan_limit
    } else {
        actual_count
    };
    let truncated = scan_limited || count > request.max_results;
    let returned = files
        .into_iter()
        .take(request.max_results)
        .collect::<Vec<_>>();

    Ok(ListFilesOutcome::new(
        returned,
        count,
        truncated,
        scan_limited,
        ignored_roots,
    ))
}
