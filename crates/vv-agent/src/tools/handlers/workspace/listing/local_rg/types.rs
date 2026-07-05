use std::path::{Path, PathBuf};

use crate::tools::base::ToolContext;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RgFindFilesResult {
    pub(super) files: Vec<String>,
    pub(super) total_count: usize,
    pub(super) truncated: bool,
    pub(super) scan_limited: bool,
    pub(super) sensitive_files_omitted: usize,
}

pub(super) struct RgFindFilesRequest<'a> {
    pub(super) context: &'a ToolContext,
    pub(super) base_path: &'a Path,
    pub(super) base_is_workspace_root: bool,
    pub(super) glob: &'a str,
    pub(super) include_hidden: bool,
    pub(super) include_ignored: bool,
    pub(super) include_sensitive: bool,
    pub(super) ignored_root_names: &'a [String],
    pub(super) scan_limit: usize,
    pub(super) rg_executable: &'a PathBuf,
}
