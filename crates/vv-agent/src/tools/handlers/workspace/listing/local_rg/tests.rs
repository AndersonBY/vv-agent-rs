use std::path::{Path, PathBuf};

use crate::tools::base::ToolContext;

use super::scan::find_files_local_rg;
use super::types::RgFindFilesRequest;

fn write_fake_rg(workspace: &Path, script: &str) -> PathBuf {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt;

    let fake_rg = workspace.join("fake-rg");
    let mut file = std::fs::File::create(&fake_rg).expect("fake rg");
    file.write_all(script.as_bytes()).expect("fake rg body");
    file.sync_all().expect("fake rg sync");
    drop(file);
    let mut permissions = std::fs::metadata(&fake_rg).expect("metadata").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&fake_rg, permissions).expect("chmod");
    fake_rg
}

#[test]
fn find_files_rg_fast_path_normalizes_dot_slash_glob_matches() {
    let workspace = tempfile::tempdir().expect("workspace");
    let fake_rg = write_fake_rg(
        workspace.path(),
        "#!/bin/sh\nprintf './doc.md\\0./nested/inner.md\\0note.txt\\0'\n",
    );

    let context = ToolContext::new(workspace.path());
    let result = find_files_local_rg(RgFindFilesRequest {
        context: &context,
        base_path: workspace.path(),
        base_is_workspace_root: true,
        glob: "*.md",
        include_hidden: false,
        include_ignored: false,
        include_sensitive: false,
        ignored_root_names: &[],
        scan_limit: 100,
        rg_executable: &fake_rg,
    })
    .expect("rg result");

    assert_eq!(result.files, vec!["doc.md"]);
    assert_eq!(result.total_count, 1);
    assert!(!result.truncated);
    assert!(!result.scan_limited);
}

#[test]
fn find_files_rg_scan_limited_count_reports_matched_items() {
    let workspace = tempfile::tempdir().expect("workspace");
    let fake_rg = write_fake_rg(
        workspace.path(),
        "#!/bin/sh\nprintf 'a.txt\\0b.txt\\0doc.md\\0late.md\\0'\n",
    );

    let context = ToolContext::new(workspace.path());
    let result = find_files_local_rg(RgFindFilesRequest {
        context: &context,
        base_path: workspace.path(),
        base_is_workspace_root: true,
        glob: "*.md",
        include_hidden: false,
        include_ignored: false,
        include_sensitive: false,
        ignored_root_names: &[],
        scan_limit: 3,
        rg_executable: &fake_rg,
    })
    .expect("rg result");

    assert_eq!(result.files, vec!["doc.md"]);
    assert_eq!(result.total_count, 1);
    assert!(result.truncated);
    assert!(result.scan_limited);
}
