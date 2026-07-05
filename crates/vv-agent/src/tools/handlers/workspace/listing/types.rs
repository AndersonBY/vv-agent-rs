#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FindFilesOutcome {
    pub(super) files: Vec<String>,
    pub(super) count: usize,
    pub(super) truncated: bool,
    pub(super) scan_limited: bool,
    pub(super) ignored_roots: Vec<String>,
    pub(super) sort: String,
    pub(super) sensitive_files_omitted: usize,
}

impl FindFilesOutcome {
    pub(super) fn new(
        files: Vec<String>,
        count: usize,
        truncated: bool,
        scan_limited: bool,
        ignored_roots: Vec<String>,
        sort: String,
        sensitive_files_omitted: usize,
    ) -> Self {
        Self {
            files,
            count,
            truncated,
            scan_limited,
            ignored_roots,
            sort,
            sensitive_files_omitted,
        }
    }
}
