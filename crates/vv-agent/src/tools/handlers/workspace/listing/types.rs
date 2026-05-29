#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ListFilesOutcome {
    pub(super) files: Vec<String>,
    pub(super) count: usize,
    pub(super) truncated: bool,
    pub(super) scan_limited: bool,
    pub(super) ignored_roots: Vec<String>,
}

impl ListFilesOutcome {
    pub(super) fn new(
        files: Vec<String>,
        count: usize,
        truncated: bool,
        scan_limited: bool,
        ignored_roots: Vec<String>,
    ) -> Self {
        Self {
            files,
            count,
            truncated,
            scan_limited,
            ignored_roots,
        }
    }
}
